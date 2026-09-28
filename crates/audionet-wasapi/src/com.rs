//! Scoped COM initialization.

use core::marker::PhantomData;

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// Keeps COM initialized on the current thread while alive.
///
/// Requests the multithreaded apartment (MTA). If the thread already belongs
/// to a single-threaded apartment (`RPC_E_CHANGED_MODE`), the existing
/// apartment is used instead: the MMDevice API works from either, and a
/// library must not change a thread apartment it does not own.
///
/// Every COM interface pointer obtained under this guard must be released
/// before the guard is dropped, because dropping it may uninitialize COM.
/// The guard is `!Send` because COM initialization is per thread.
pub(crate) struct ComApartment {
    owns_initialization: bool,
    _not_send: PhantomData<*const ()>,
}

impl ComApartment {
    pub(crate) fn enter() -> windows::core::Result<Self> {
        // SAFETY: the reserved parameter must be null (`None`). A successful
        // call (S_OK or S_FALSE) is balanced by `CoUninitialize` in `Drop`
        // on this same thread, which `!Send` guarantees.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self {
                owns_initialization: false,
                _not_send: PhantomData,
            });
        }
        hr.ok()?;
        Ok(Self {
            owns_initialization: true,
            _not_send: PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owns_initialization {
            // SAFETY: balances the successful `CoInitializeEx` in `enter` on
            // the same thread.
            unsafe { CoUninitialize() };
        }
    }
}
