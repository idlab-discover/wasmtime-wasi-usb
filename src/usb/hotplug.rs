use log::{debug, warn};
use once_cell::sync::Lazy;
use std::collections::VecDeque;
use std::sync::Mutex;

use wasmtime::component::Resource;

use libusb1_sys::{
    constants::*, libusb_context, libusb_device, libusb_has_capability,
    libusb_hotplug_callback_handle, libusb_hotplug_register_callback, libusb_ref_device,
};

use crate::usb::WasiUsbCtxView;
use crate::usb::bindings::wasi::usb;
use usb::device::UsbDevice;
use usb::errors::LibusbError;
use usb::hotplug::{Event, Info};

static HOTPLUG_QUEUE: Lazy<Mutex<VecDeque<(Event, Info, UsbDevice)>>> =
    Lazy::new(|| Mutex::new(VecDeque::new()));

impl usb::hotplug::Host for WasiUsbCtxView<'_> {
    fn enable_hotplug(&mut self) -> Result<(), LibusbError> {
        if self.ctx.hotplug_enabled {
            return Ok(());
        }
        unsafe {
            if libusb_has_capability(LIBUSB_CAP_HAS_HOTPLUG) == 0 {
                // no hotplug support
                return Err(LibusbError::NotSupported);
            }

            let mut handle: libusb_hotplug_callback_handle = 0;
            let rc = libusb_hotplug_register_callback(
                self.ctx.libusb_ctx.ok_or(LibusbError::NotFound)?,
                LIBUSB_HOTPLUG_EVENT_DEVICE_ARRIVED | LIBUSB_HOTPLUG_EVENT_DEVICE_LEFT,
                LIBUSB_HOTPLUG_NO_FLAGS,
                LIBUSB_HOTPLUG_MATCH_ANY,
                LIBUSB_HOTPLUG_MATCH_ANY,
                LIBUSB_HOTPLUG_MATCH_ANY,
                hotplug_cb,
                std::ptr::null_mut(),
                &mut handle,
            );
            if rc < 0 {
                return Err(LibusbError::from_raw(rc));
            }
            self.ctx.hotplug_handle = Some(handle);
            self.ctx.hotplug_enabled = true;
        }

        Ok(())
    }

    fn poll_events(&mut self) -> Vec<(Event, Info, Resource<UsbDevice>)> {
        let mut q = HOTPLUG_QUEUE.lock().unwrap();
        let mut out = Vec::with_capacity(q.len());
        while let Some(ev) = q.pop_front() {
            let device = self.table.push(ev.2).or(Err(LibusbError::Other)).unwrap();
            let ev2 = (ev.0, ev.1, device);
            out.push(ev2);
        }
        out
    }
}

extern "system" fn hotplug_cb(
    _: *mut libusb_context,
    dev: *mut libusb_device,
    ev: libusb1_sys::libusb_hotplug_event,
    _user_data: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    debug!("hotplug_cb called with event code: {:?}", ev);
    unsafe {
        // gather minimal info WITHOUT opening the device
        let mut desc = std::mem::MaybeUninit::<libusb1_sys::libusb_device_descriptor>::uninit();
        if libusb1_sys::libusb_get_device_descriptor(dev, desc.as_mut_ptr()) != 0 {
            log::error!("Failed to get device descriptor");
            return 0; // ignore
        }
        let desc = desc.assume_init();

        let bus = libusb1_sys::libusb_get_bus_number(dev);
        let addr = libusb1_sys::libusb_get_device_address(dev);
        debug!(
            "Device details - bus: {}, address: {}, vendor: {:#06x}, product: {:#06x}",
            bus, addr, desc.idVendor, desc.idProduct
        );

        let info = Info {
            bus,
            address: addr,
            vendor: desc.idVendor,
            product: desc.idProduct,
        };
        let event = match ev {
            LIBUSB_HOTPLUG_EVENT_DEVICE_ARRIVED => {
                log::info!("Device arrived: {:?}", info);
                Event::ARRIVED
            }
            LIBUSB_HOTPLUG_EVENT_DEVICE_LEFT => {
                log::info!("Device left: {:?}", info);
                Event::LEFT
            }
            _ => {
                warn!("Unknown hotplug event: {:?}", ev);
                return 0;
            }
        };

        // Need to increase refcount before storing in queue
        libusb_ref_device(dev); // Add this line to increment reference count

        let mut q = HOTPLUG_QUEUE.lock().unwrap();
        q.push_back((event, info, UsbDevice { device: dev }));
        debug!("Hotplug event pushed to queue");
        0
    }
}
