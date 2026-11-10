# Enables or disables one audio endpoint the way the Sound control panel's
# "Disable" does (IPolicyConfig::SetEndpointVisibility), without touching
# its driver or the computer's other endpoints. Needs an administrator.
#   endpoint_visibility.ps1 -Id '{0.0.0.00000000}.{...}' -Visible 0|1
param([Parameter(Mandatory)][string]$Id, [Parameter(Mandatory)][int]$Visible)
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
[ComImport, Guid("f8679f50-850a-41cf-9c72-430f290290c8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IPolicyConfig {
    int GetMixFormat(); int GetDeviceFormat(); int ResetDeviceFormat(); int SetDeviceFormat();
    int GetProcessingPeriod(); int SetProcessingPeriod(); int GetShareMode(); int SetShareMode();
    int GetPropertyValue(); int SetPropertyValue(); int SetDefaultEndpoint();
    [PreserveSig] int SetEndpointVisibility([MarshalAs(UnmanagedType.LPWStr)] string id, int visible);
}
[ComImport, Guid("870af99c-171d-4f9e-af0d-e63df40c2bc9")] class CPolicyConfigClient {}
public static class EndpointVisibility {
    public static int Set(string id, bool visible) {
        return ((IPolicyConfig)new CPolicyConfigClient()).SetEndpointVisibility(id, visible ? 1 : 0);
    }
}
'@
$hr = [EndpointVisibility]::Set($Id, [bool]$Visible)
if ($hr -ne 0) { throw ("SetEndpointVisibility failed: 0x{0:X8}" -f $hr) }
