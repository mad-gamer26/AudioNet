//! Endpoint enumeration through `IMMDeviceEnumerator`.
//!
//! Enumeration reads only the endpoint ID, state, data flow and property
//! store. It never activates an `IAudioClient`, so listing devices cannot
//! open streams, wake Bluetooth hands-free profiles, or otherwise disturb
//! audio that is already playing. See `docs/windows-audio.md`.

use audionet_audio::{BackendError, EndpointInventory, EnumerationOptions, EnumerationWarning};
use audionet_protocol::{
    AudioBackend, DefaultRole, DeviceFormat, Direction, EndpointDescriptor, EndpointId,
    EndpointState, LoopbackSupport,
};
use windows::Win32::Devices::FunctionDiscovery::{
    PKEY_Device_DeviceDesc, PKEY_Device_FriendlyName, PKEY_DeviceInterface_FriendlyName,
};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATE_DISABLED, DEVICE_STATE_NOTPRESENT,
    DEVICE_STATE_UNPLUGGED, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator, IMMEndpoint,
    MMDeviceEnumerator, PKEY_AudioEngine_DeviceFormat, eAll, eCapture, eCommunications, eConsole,
    eMultimedia, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PropVariantClear, PropVariantToStringAlloc,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::Win32::System::Variant::{VT_BLOB, VT_EMPTY};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::core::{Interface, PWSTR};

use crate::com::ComApartment;
use crate::waveformat::parse_waveformat;

pub(crate) fn enumerate(options: EnumerationOptions) -> Result<EndpointInventory, BackendError> {
    let _com = ComApartment::enter().map_err(|e| backend_error("initializing COM", &e))?;
    // All COM interfaces live inside this call, so they are released before
    // `_com` is dropped.
    enumerate_in_apartment(options)
}

fn enumerate_in_apartment(options: EnumerationOptions) -> Result<EndpointInventory, BackendError> {
    // SAFETY: COM is initialized on this thread for the duration of the call
    // (see `enumerate`). `MMDeviceEnumerator` is the documented CLSID for
    // `IMMDeviceEnumerator`.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| backend_error("creating the audio device enumerator", &e))?;

    let mut mask = DEVICE_STATE_ACTIVE.0 | DEVICE_STATE_DISABLED.0 | DEVICE_STATE_UNPLUGGED.0;
    if options.include_not_present {
        mask |= DEVICE_STATE_NOTPRESENT.0;
    }
    // SAFETY: `enumerator` is a valid interface; the arguments are documented
    // enum and bitmask values.
    let collection = unsafe { enumerator.EnumAudioEndpoints(eAll, DEVICE_STATE(mask)) }
        .map_err(|e| backend_error("listing audio endpoints", &e))?;
    // SAFETY: `collection` is a valid interface.
    let count = unsafe { collection.GetCount() }
        .map_err(|e| backend_error("counting audio endpoints", &e))?;

    let defaults = read_default_endpoints(&enumerator);
    let mut endpoints = Vec::with_capacity(count as usize);
    let mut warnings = Vec::new();

    for index in 0..count {
        // SAFETY: `index` is within the count the collection reported. If a
        // device vanished in between, the call fails and is reported below.
        match unsafe { collection.Item(index) } {
            Ok(device) => {
                if let Some(endpoint) = describe_endpoint(&device, &defaults, &mut warnings) {
                    endpoints.push(endpoint);
                }
            }
            Err(e) => warnings.push(EnumerationWarning {
                native_id: None,
                message: format!(
                    "Could not open endpoint {} of {count}: {}",
                    index + 1,
                    hresult_text(&e)
                ),
            }),
        }
    }

    Ok(EndpointInventory::new(endpoints, warnings))
}

/// Reads the IDs of the current default endpoints for every flow and role.
/// A missing default (for example, no microphone at all) is normal and is
/// not reported.
fn read_default_endpoints(enumerator: &IMMDeviceEnumerator) -> Vec<(String, DefaultRole)> {
    const ROLES: [(ERole, DefaultRole); 3] = [
        (eConsole, DefaultRole::Console),
        (eMultimedia, DefaultRole::Multimedia),
        (eCommunications, DefaultRole::Communications),
    ];
    let mut defaults = Vec::new();
    for flow in [eRender, eCapture] {
        for (role, role_value) in ROLES {
            // SAFETY: `enumerator` is valid; arguments are documented values.
            // Failure (typically E_NOTFOUND) just means there is no default.
            if let Ok(device) = unsafe { enumerator.GetDefaultAudioEndpoint(flow, role) } {
                if let Ok(id) = device_id(&device) {
                    defaults.push((id, role_value));
                }
            }
        }
    }
    defaults
}

/// Builds a descriptor for one device. Returns `None` (with a warning) only
/// if the device cannot be identified at all; missing optional properties
/// become warnings on an otherwise complete descriptor.
fn describe_endpoint(
    device: &IMMDevice,
    defaults: &[(String, DefaultRole)],
    warnings: &mut Vec<EnumerationWarning>,
) -> Option<EndpointDescriptor> {
    let native_id = match device_id(device) {
        Ok(id) => id,
        Err(e) => {
            warnings.push(EnumerationWarning {
                native_id: None,
                message: format!(
                    "Could not read an endpoint identifier: {}",
                    hresult_text(&e)
                ),
            });
            return None;
        }
    };
    let mut warn = |message: String| {
        warnings.push(EnumerationWarning {
            native_id: Some(native_id.clone()),
            message,
        });
    };

    let id = match EndpointId::new(AudioBackend::Wasapi, native_id.clone()) {
        Ok(id) => id,
        Err(e) => {
            warn(format!("Endpoint identifier is not usable: {e}"));
            return None;
        }
    };

    // SAFETY: `device` is a valid interface.
    let state = match unsafe { device.GetState() } {
        Ok(state) => match endpoint_state(state) {
            Some(state) => state,
            None => {
                warn(format!("Endpoint reported unknown state 0x{:X}", state.0));
                return None;
            }
        },
        Err(e) => {
            warn(format!(
                "Could not read endpoint state: {}",
                hresult_text(&e)
            ));
            return None;
        }
    };

    let direction = match data_flow(device) {
        Ok(direction) => direction,
        Err(message) => {
            warn(message);
            return None;
        }
    };

    let mut name = None;
    let mut description = None;
    let mut adapter = None;
    let mut format = None;
    // SAFETY: `device` is a valid interface; STGM_READ requests read-only
    // access to the endpoint's property store.
    match unsafe { device.OpenPropertyStore(STGM_READ) } {
        Ok(store) => {
            let mut read = |key: &PROPERTYKEY, what: &str| match read_string(&store, key) {
                Ok(value) => value,
                Err(e) => {
                    warn(format!("Could not read {what}: {}", hresult_text(&e)));
                    None
                }
            };
            name = read(&PKEY_Device_FriendlyName, "the endpoint name");
            description = read(&PKEY_Device_DeviceDesc, "the endpoint description");
            adapter = read(&PKEY_DeviceInterface_FriendlyName, "the adapter name");
            match read_device_format(&store) {
                Ok(value) => format = value,
                Err(message) => warn(format!("Could not read the device format: {message}")),
            }
        }
        Err(e) => warn(format!(
            "Could not open the endpoint property store: {}",
            hresult_text(&e)
        )),
    }

    let name = name
        .or_else(|| description.clone())
        .unwrap_or_else(|| format!("Unnamed {} endpoint", direction.label().to_lowercase()));

    let mut default_roles: Vec<DefaultRole> = defaults
        .iter()
        .filter(|(default_id, _)| *default_id == native_id)
        .map(|(_, role)| *role)
        .collect();
    default_roles.sort();
    default_roles.dedup();

    Some(EndpointDescriptor {
        id,
        direction,
        name,
        description,
        adapter,
        state,
        default_roles,
        format,
        loopback: loopback_support(direction, state),
    })
}

/// Enumeration-time loopback knowledge. WASAPI documents loopback capture
/// for shared-mode render endpoints, so an active output endpoint is
/// `Expected` to support it; nothing stronger is claimed without a probe.
fn loopback_support(direction: Direction, state: EndpointState) -> LoopbackSupport {
    match (direction, state) {
        (Direction::Input, _) => LoopbackSupport::NotApplicable,
        (Direction::Output, EndpointState::Active) => LoopbackSupport::Expected,
        (Direction::Output, _) => LoopbackSupport::EndpointNotActive,
    }
}

pub(crate) fn endpoint_state(state: DEVICE_STATE) -> Option<EndpointState> {
    Some(match state {
        DEVICE_STATE_ACTIVE => EndpointState::Active,
        DEVICE_STATE_DISABLED => EndpointState::Disabled,
        DEVICE_STATE_NOTPRESENT => EndpointState::NotPresent,
        DEVICE_STATE_UNPLUGGED => EndpointState::Unplugged,
        _ => return None,
    })
}

pub(crate) fn data_flow(device: &IMMDevice) -> Result<Direction, String> {
    let endpoint: IMMEndpoint = device
        .cast()
        .map_err(|e| format!("Endpoint has no data-flow interface: {}", hresult_text(&e)))?;
    // SAFETY: `endpoint` is a valid interface.
    let flow: EDataFlow = unsafe { endpoint.GetDataFlow() }
        .map_err(|e| format!("Could not read endpoint data flow: {}", hresult_text(&e)))?;
    match flow {
        f if f == eRender => Ok(Direction::Output),
        f if f == eCapture => Ok(Direction::Input),
        other => Err(format!("Endpoint reported unknown data flow {}", other.0)),
    }
}

fn device_id(device: &IMMDevice) -> windows::core::Result<String> {
    // SAFETY: `device` is a valid interface. On success `GetId` returns a
    // CoTaskMemAlloc'd string, which `take_co_string` frees exactly once.
    let raw = unsafe { device.GetId() }?;
    // SAFETY: `raw` came from `GetId` and is not used afterwards.
    Ok(unsafe { take_co_string(raw) })
}

/// Converts and frees a COM-allocated wide string.
///
/// # Safety
///
/// `raw` must be null or a NUL-terminated string allocated with
/// `CoTaskMemAlloc` that the caller owns and does not use again.
unsafe fn take_co_string(raw: PWSTR) -> String {
    if raw.is_null() {
        return String::new();
    }
    // SAFETY: guaranteed NUL-terminated by the caller. Invalid UTF-16 is
    // replaced rather than failing, since this text is only displayed or
    // compared.
    let text = unsafe { String::from_utf16_lossy(raw.as_wide()) };
    // SAFETY: the caller transfers ownership of the allocation to us.
    unsafe { CoTaskMemFree(Some(raw.0 as *const _)) };
    text
}

/// Owns a `PROPVARIANT` and clears it on drop (the `windows` crate's
/// `PROPVARIANT` does not free its contents itself).
struct PropVariant(PROPVARIANT);

impl PropVariant {
    fn vt(&self) -> u16 {
        // SAFETY: every PROPVARIANT, including a zeroed one, has a valid `vt`
        // tag in this position of the union.
        unsafe { self.0.Anonymous.Anonymous.vt.0 }
    }
}

impl Drop for PropVariant {
    fn drop(&mut self) {
        // SAFETY: the value came from `IPropertyStore::GetValue` and is owned
        // solely by this wrapper. Clearing cannot meaningfully fail here and
        // there is nothing useful to do if it did.
        let _ = unsafe { PropVariantClear(&mut self.0) };
    }
}

fn get_value(store: &IPropertyStore, key: &PROPERTYKEY) -> windows::core::Result<PropVariant> {
    // SAFETY: `store` is valid and `key` points to a live PROPERTYKEY.
    unsafe { store.GetValue(key) }.map(PropVariant)
}

fn read_string(store: &IPropertyStore, key: &PROPERTYKEY) -> windows::core::Result<Option<String>> {
    let value = get_value(store, key)?;
    if value.vt() == VT_EMPTY.0 {
        return Ok(None);
    }
    // SAFETY: `value.0` is an initialized PROPVARIANT. The returned string is
    // CoTaskMemAlloc'd and freed by `take_co_string`.
    let raw = unsafe { PropVariantToStringAlloc(&value.0) }?;
    // SAFETY: `raw` came from `PropVariantToStringAlloc` and is not reused.
    let text = unsafe { take_co_string(raw) };
    let text = text.trim().to_owned();
    Ok((!text.is_empty()).then_some(text))
}

fn read_device_format(store: &IPropertyStore) -> Result<Option<DeviceFormat>, String> {
    let value = get_value(store, &PKEY_AudioEngine_DeviceFormat).map_err(|e| hresult_text(&e))?;
    let vt = value.vt();
    if vt == VT_EMPTY.0 {
        return Ok(None);
    }
    if vt != VT_BLOB.0 {
        return Err(format!("unexpected property type {vt}"));
    }
    // SAFETY: `vt` is VT_BLOB, so the `blob` union member is active.
    let blob = unsafe { value.0.Anonymous.Anonymous.Anonymous.blob };
    if blob.pBlobData.is_null() || blob.cbSize == 0 {
        return Ok(None);
    }
    // SAFETY: for VT_BLOB, `pBlobData` points to `cbSize` readable bytes
    // owned by `value`, which outlives this slice.
    let bytes = unsafe { core::slice::from_raw_parts(blob.pBlobData, blob.cbSize as usize) };
    parse_waveformat(bytes).map(Some).map_err(|e| e.to_string())
}

pub(crate) fn hresult_text(e: &windows::core::Error) -> String {
    let message = e.message();
    let message = message.trim();
    let code = e.code().0 as u32;
    if message.is_empty() {
        format!("HRESULT 0x{code:08X}")
    } else {
        format!("{message} (HRESULT 0x{code:08X})")
    }
}

fn backend_error(operation: &str, e: &windows::core::Error) -> BackendError {
    BackendError {
        backend: AudioBackend::Wasapi,
        operation: operation.into(),
        detail: hresult_text(e),
    }
}
