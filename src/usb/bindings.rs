pub use crate::usb::device::{UsbDevice, UsbDeviceHandle};
pub use crate::usb::transfers::UsbTransfer;

wasmtime::component::bindgen!({
    with: {
        "wasi:usb/transfers.transfer": UsbTransfer,
        "wasi:usb/device.usb-device": UsbDevice,
        "wasi:usb/device.device-handle": UsbDeviceHandle,
    },
    imports: {
        "wasi:usb/transfers.await-transfer": async,
    }
});
