use crate::usb::bindings::wasi::usb::errors::LibusbError;

impl LibusbError {
    /// Convert a raw `libusb_error` integer value to a `LibusbError` variant.
    pub fn from_raw(value: i32) -> Self {
        match value {
            -1 => LibusbError::Io,
            -2 => LibusbError::InvalidParam,
            -3 => LibusbError::Access,
            -4 => LibusbError::NoDevice,
            -5 => LibusbError::NotFound,
            -6 => LibusbError::Busy,
            -7 => LibusbError::Timeout,
            -8 => LibusbError::Overflow,
            -9 => LibusbError::Pipe,
            -10 => LibusbError::Interrupted,
            -11 => LibusbError::NoMem,
            -12 => LibusbError::NotSupported,
            -99 => LibusbError::Other,
            _ => LibusbError::Other, // Default to `Other` for unknown error codes
        }
    }
}

impl crate::usb::bindings::wasi::usb::errors::Host for crate::usb::WasiUsbCtxView<'_> {}
