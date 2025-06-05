#[cfg(target_os = "windows")]
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator};
#[cfg(target_os = "windows")]
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
#[cfg(target_os = "windows")]
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

#[cfg(target_os = "windows")]
fn get_default_endpoint() -> Result<IAudioEndpointVolume, String> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .map_err(|e| format!("CoInitializeEx failed: {e}"))?;
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| format!("CoCreateInstance failed: {e}"))?;
        let device: IMMDevice = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| format!("GetDefaultAudioEndpoint failed: {e}"))?;
        let endpoint: IAudioEndpointVolume = device
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .map_err(|e| format!("Activate failed: {e}"))?;
        Ok(endpoint)
    }
}

#[cfg(target_os = "windows")]
/// Sets the master system volume on Windows.
/// `value` should be between 0 and 100.
pub fn set_volume(value: u32) -> Result<(), String> {
    if value > 100 {
        return Err("Volume must be between 0 and 100".to_string());
    }
    unsafe {
        let endpoint = get_default_endpoint()?;
        let scalar = (value as f32) / 100.0;
        endpoint
            .SetMasterVolumeLevelScalar(scalar, std::ptr::null())
            .map_err(|e| format!("SetMasterVolumeLevelScalar failed: {e}"))?;
        CoUninitialize();
    }
    Ok(())
}

#[cfg(target_os = "windows")]
/// Returns the current master system volume on Windows as a value from 0 to 100.
pub fn get_volume() -> Result<u32, String> {
    unsafe {
        let endpoint = get_default_endpoint()?;
        let vol = endpoint
            .GetMasterVolumeLevelScalar()
            .map_err(|e| format!("GetMasterVolumeLevelScalar failed: {e}"))?;
        CoUninitialize();
        Ok((vol * 100.0).round() as u32)
    }
}

#[cfg(not(target_os = "windows"))]
/// Stub for non-Windows platforms.
pub fn set_volume(_value: u32) -> Result<(), String> {
    Err("set_volume is only supported on Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
/// Stub for non-Windows platforms.
pub fn get_volume() -> Result<u32, String> {
    Err("get_volume is only supported on Windows".to_string())
}
