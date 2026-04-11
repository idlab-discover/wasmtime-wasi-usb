mod bindings;
mod configuration;
mod descriptors;
mod device;
mod errors;
mod hotplug;
mod transfers;

use std::{
    sync::{Arc, atomic::AtomicBool},
    thread,
};

use libusb1_sys::{libusb_context, libusb_hotplug_callback_handle};
use wasmtime::component::{HasData, Linker};
use wasmtime_wasi::ResourceTable;

pub struct WasiUsbCtx {
    libusb_ctx: Option<*mut libusb_context>,
    event_loop_flag: Option<Arc<AtomicBool>>,
    event_thread: Option<thread::JoinHandle<()>>,
    hotplug_enabled: bool,
    hotplug_handle: Option<libusb_hotplug_callback_handle>,
}
unsafe impl Send for WasiUsbCtx {}

impl WasiUsbCtx {
    pub fn new(hotplug_enabled: bool) -> Self {
        Self {
            libusb_ctx: None,
            event_loop_flag: None,
            event_thread: None,
            hotplug_enabled,
            hotplug_handle: None,
        }
    }
}

pub struct WasiUsbCtxView<'a> {
    pub ctx: &'a mut WasiUsbCtx,
    pub table: &'a mut ResourceTable,
}

pub trait WasiUsbView {
    fn usb(&mut self) -> WasiUsbCtxView<'_>;
}

struct WasiUsb;

impl HasData for WasiUsb {
    type Data<'a> = WasiUsbCtxView<'a>;
}

pub fn add_to_linker<T: WasiUsbView + Send + 'static>(
    linker: &mut Linker<T>,
) -> wasmtime::Result<()> {
    bindings::wasi::usb::device::add_to_linker::<T, WasiUsb>(linker, T::usb)?;
    bindings::wasi::usb::transfers::add_to_linker::<T, WasiUsb>(linker, T::usb)?;
    bindings::wasi::usb::hotplug::add_to_linker::<T, WasiUsb>(linker, T::usb)?;
    Ok(())
}
