//! Temporarily restore input audio while a transcription is recording.
//!
//! This is separate from Handy's "Mute While Recording" option, which mutes
//! OUTPUT audio. On macOS we modify the selected CoreAudio input device.
//! Devices that expose neither a writable input mute nor input level cannot
//! be controlled by the OS, so we leave them unchanged and log a warning.

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{c_char, c_void, CStr};
    use std::mem::size_of;
    use std::ptr;

    use log::{debug, warn};

    type AudioDeviceId = u32;
    const SYSTEM_OBJECT: u32 = 1;
    const MAIN: u32 = 0;
    const GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    const INPUT: u32 = u32::from_be_bytes(*b"inpt");
    const DEFAULT_INPUT: u32 = u32::from_be_bytes(*b"dIn ");
    const ALL_DEVICES: u32 = u32::from_be_bytes(*b"dev#");
    const DEVICE_NAME: u32 = u32::from_be_bytes(*b"lnam");
    const DEVICE_MUTE: u32 = u32::from_be_bytes(*b"mute");
    const DEVICE_VOLUME: u32 = u32::from_be_bytes(*b"volm");
    const UTF8: u32 = 0x0800_0100;
    // If the user muted by setting input gain to zero (rather than using a
    // mute switch), CoreAudio doesn't retain the earlier gain. Choose a
    // usable recording level and restore zero immediately after capture.
    const RECORDING_INPUT_GAIN: f32 = 0.75;

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
        fn AudioObjectGetPropertyDataSize(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
        ) -> i32;
        fn AudioObjectHasProperty(object: u32, address: *const PropertyAddress) -> u8;
        fn AudioObjectIsPropertySettable(
            object: u32,
            address: *const PropertyAddress,
            settable: *mut u8,
        ) -> i32;
        fn AudioObjectSetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: u32,
            data: *const c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(value: *const c_void);
    }

    fn address(selector: u32, scope: u32, element: u32) -> PropertyAddress {
        PropertyAddress {
            selector,
            scope,
            element,
        }
    }

    fn get<T: Default>(object: u32, property: PropertyAddress) -> Result<T, String> {
        let mut value = T::default();
        let mut size = size_of::<T>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &property,
                0,
                ptr::null(),
                &mut size,
                (&mut value as *mut T).cast(),
            )
        };
        if status != 0 || size != size_of::<T>() as u32 {
            return Err(format!("CoreAudio read failed (status {status})"));
        }
        Ok(value)
    }

    fn set<T>(object: u32, property: PropertyAddress, value: T) -> Result<(), String> {
        let status = unsafe {
            AudioObjectSetPropertyData(
                object,
                &property,
                0,
                ptr::null(),
                size_of::<T>() as u32,
                (&value as *const T).cast(),
            )
        };
        if status != 0 {
            Err(format!("CoreAudio write failed (status {status})"))
        } else {
            Ok(())
        }
    }

    fn writable(object: u32, property: &PropertyAddress) -> bool {
        if unsafe { AudioObjectHasProperty(object, property) } == 0 {
            return false;
        }
        let mut settable = 0u8;
        unsafe { AudioObjectIsPropertySettable(object, property, &mut settable) } == 0
            && settable != 0
    }

    fn control_elements(device: AudioDeviceId, selector: u32) -> Vec<u32> {
        // Some interfaces expose a master control, others expose per-channel
        // controls. Never apply both: a master usually governs all channels.
        if writable(device, &address(selector, INPUT, MAIN)) {
            return vec![MAIN];
        }
        (1..=8)
            .filter(|&element| writable(device, &address(selector, INPUT, element)))
            .collect()
    }

    fn device_name(device: AudioDeviceId) -> Result<String, String> {
        let raw: *const c_void = get(device, address(DEVICE_NAME, GLOBAL, MAIN))?;
        if raw.is_null() {
            return Err("CoreAudio returned an empty device name".into());
        }
        let mut buffer = [0 as c_char; 1024];
        let ok = unsafe {
            CFStringGetCString(raw, buffer.as_mut_ptr(), buffer.len() as isize, UTF8)
        };
        // CoreAudio gives the caller an owned CFStringRef for this property.
        unsafe { CFRelease(raw) };
        if ok == 0 {
            return Err("Could not decode CoreAudio microphone name".into());
        }
        Ok(unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned())
    }

    fn selected_device(name: Option<&str>) -> Result<AudioDeviceId, String> {
        let Some(name) = name.filter(|name| !name.is_empty()) else {
            return get(SYSTEM_OBJECT, address(DEFAULT_INPUT, GLOBAL, MAIN));
        };

        let mut size = 0u32;
        let property = address(ALL_DEVICES, GLOBAL, MAIN);
        let status = unsafe {
            AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &property, 0, ptr::null(), &mut size)
        };
        if status != 0 || size % size_of::<AudioDeviceId>() as u32 != 0 {
            return Err(format!("Cannot enumerate CoreAudio devices ({status})"));
        }
        let mut devices = vec![0u32; size as usize / size_of::<AudioDeviceId>()];
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &property,
                0,
                ptr::null(),
                &mut size,
                devices.as_mut_ptr().cast(),
            )
        };
        if status != 0 {
            return Err(format!("Cannot read CoreAudio devices ({status})"));
        }
        let matches: Vec<_> = devices
            .into_iter()
            .filter(|&device| device_name(device).as_deref() == Ok(name))
            .collect();
        match matches.as_slice() {
            [device] => Ok(*device),
            [] => Err(format!(
                "Selected microphone '{name}' is not available in CoreAudio"
            )),
            _ => Err(format!(
                "Multiple microphones are named '{name}'; refusing to alter an ambiguous device"
            )),
        }
    }

    enum SavedControl {
        Mute { element: u32, value: u32 },
        Volume { element: u32, value: f32 },
    }

    /// The original input state and physical device id, not just the current
    /// default. Drop restores even when an error path abandons the recording.
    pub struct InputMuteSnapshot {
        device: AudioDeviceId,
        changed: Vec<SavedControl>,
    }

    impl Drop for InputMuteSnapshot {
        fn drop(&mut self) {
            for control in self.changed.iter().rev() {
                let result = match *control {
                    SavedControl::Mute { element, value } => {
                        set(self.device, address(DEVICE_MUTE, INPUT, element), value)
                    }
                    SavedControl::Volume { element, value } => {
                        set(self.device, address(DEVICE_VOLUME, INPUT, element), value)
                    }
                };
                if let Err(error) = result {
                    warn!("Could not restore microphone input state: {error}");
                }
            }
        }
    }

    /// Restore an input that was already muted when the shortcut was pressed.
    /// An already-unmuted device is left alone on both start and stop.
    pub fn unmute_for_recording(name: Option<&str>) -> Result<Option<InputMuteSnapshot>, String> {
        let device = selected_device(name)?;
        if device == 0 {
            return Err("No CoreAudio input device is selected".into());
        }
        let mut snapshot = InputMuteSnapshot {
            device,
            changed: Vec::new(),
        };
        let mut supported = false;

        for element in control_elements(device, DEVICE_MUTE) {
            let property = address(DEVICE_MUTE, INPUT, element);
            let was_muted: u32 = get(device, property)?;
            supported = true;
            if was_muted != 0 {
                set(device, property, 0u32)?;
                snapshot.changed.push(SavedControl::Mute {
                    element,
                    value: was_muted,
                });
            }
        }

        // A zero input level is another common way to mute a Mac microphone.
        // Restore that exact level after recording, even if there is no mute
        // switch on this device.
        for element in control_elements(device, DEVICE_VOLUME) {
            let property = address(DEVICE_VOLUME, INPUT, element);
            let previous: f32 = get(device, property)?;
            supported = true;
            if previous <= f32::EPSILON {
                set(device, property, RECORDING_INPUT_GAIN)?;
                snapshot.changed.push(SavedControl::Volume {
                    element,
                    value: previous,
                });
            }
        }

        if !supported {
            return Err(
                "Selected microphone exposes no writable CoreAudio input mute or gain control"
                    .into(),
            );
        }
        if snapshot.changed.is_empty() {
            Ok(None)
        } else {
            debug!("Temporarily unmuted CoreAudio microphone {device}");
            Ok(Some(snapshot))
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::{unmute_for_recording, InputMuteSnapshot};

#[cfg(not(target_os = "macos"))]
pub struct InputMuteSnapshot;

#[cfg(not(target_os = "macos"))]
pub fn unmute_for_recording(
    _selected_microphone: Option<&str>,
) -> Result<Option<InputMuteSnapshot>, String> {
    Ok(None)
}
