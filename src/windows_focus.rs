#[cfg(target_os = "windows")]
use windows::Win32::Foundation::HWND;
#[cfg(target_os = "windows")]
use windows::Win32::System::Console::GetConsoleWindow;
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_RESTORE};

#[cfg(target_os = "windows")]
/// Brings the console window to the foreground on Windows.
pub fn bring_terminal_to_front() {
    unsafe {
        let hwnd: HWND = GetConsoleWindow();
        if hwnd.0 != 0 {
            // Restore the window in case it is minimized
            ShowWindow(hwnd, SW_RESTORE);
            // Attempt to bring the window to the foreground
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(not(target_os = "windows"))]
/// Stub for non-Windows platforms.
pub fn bring_terminal_to_front() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn call_bring_terminal_to_front() {
        // The function should simply run without panicking.
        bring_terminal_to_front();
    }
}

