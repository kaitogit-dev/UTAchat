use std::{ffi::c_void, ptr, thread, time::Duration};

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn FindWindowW(class_name: *const u16, window_name: *const u16) -> *mut c_void;
}

#[cfg(windows)]
#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(hwnd: *mut c_void, attribute: u32, value: *const c_void, length: u32) -> i32;
}

#[cfg(windows)]
fn set_attribute(hwnd: *mut c_void, attribute: u32, value: u32) {
    unsafe {
        DwmSetWindowAttribute(hwnd, attribute, (&value as *const u32).cast(), std::mem::size_of::<u32>() as u32);
    }
}

#[cfg(windows)]
fn color_ref(rgb: u32) -> u32 {
    ((rgb & 0x0000ff) << 16) | (rgb & 0x00ff00) | ((rgb & 0xff0000) >> 16)
}

#[cfg(windows)]
pub fn apply() {
    thread::spawn(|| {
        let caption: Vec<u16> = "UTAchat".encode_utf16().chain(std::iter::once(0)).collect();
        for _ in 0..24 {
            let hwnd = unsafe { FindWindowW(ptr::null(), caption.as_ptr()) };
            if !hwnd.is_null() {
                set_attribute(hwnd, 20, 1);
                set_attribute(hwnd, 35, color_ref(crate::resources::theme_color("rail")));
                set_attribute(hwnd, 36, color_ref(crate::resources::theme_color("text")));
                set_attribute(hwnd, 34, color_ref(crate::resources::theme_color("rail")));
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
    });
}

#[cfg(not(windows))]
pub fn apply() {}
