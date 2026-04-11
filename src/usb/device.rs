use libc::timeval;
use log::{debug, error, info, trace, warn};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use libusb1_sys::{constants::*, libusb_transfer_set_stream_id};
use libusb1_sys::{
    libusb_alloc_streams, libusb_alloc_transfer, libusb_attach_kernel_driver,
    libusb_claim_interface, libusb_clear_halt, libusb_close, libusb_config_descriptor,
    libusb_context, libusb_detach_kernel_driver, libusb_device, libusb_device_handle,
    libusb_free_config_descriptor, libusb_free_device_list, libusb_free_streams,
    libusb_get_active_config_descriptor, libusb_get_bus_number, libusb_get_config_descriptor,
    libusb_get_config_descriptor_by_value, libusb_get_configuration, libusb_get_device_address,
    libusb_get_device_descriptor, libusb_get_device_list, libusb_get_device_speed,
    libusb_get_port_number, libusb_handle_events_timeout_completed, libusb_init,
    libusb_kernel_driver_active, libusb_open, libusb_release_interface, libusb_reset_device,
    libusb_set_configuration, libusb_set_interface_alt_setting, libusb_unref_device,
};

use wasmtime::component::Resource;

use crate::usb::WasiUsbCtxView;
use crate::usb::bindings::wasi::usb;
use crate::usb::transfers::UsbTransfer;
use usb::configuration::ConfigValue;
use usb::descriptors::{
    ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor, InterfaceDescriptor,
};
use usb::device::{DeviceLocation, UsbSpeed};
use usb::errors::LibusbError;
use usb::transfers::{Transfer, TransferOptions, TransferSetup, TransferType};

pub struct UsbDevice {
    pub device: *mut libusb_device,
}
unsafe impl Send for UsbDevice {}

pub struct UsbDeviceHandle {
    pub handle: *mut libusb_device_handle,
}
unsafe impl Send for UsbDeviceHandle {}

impl UsbSpeed {
    pub fn from_raw(value: u8) -> Self {
        match value {
            0 => UsbSpeed::Unknown,
            1 => UsbSpeed::Low,
            2 => UsbSpeed::Full,
            3 => UsbSpeed::High,
            4 => UsbSpeed::Super,
            5 => UsbSpeed::SuperPlus,
            6 => UsbSpeed::SuperPlusX2,
            _ => UsbSpeed::Unknown,
        }
    }
}

impl usb::device::Host for WasiUsbCtxView<'_> {
    fn init(&mut self) -> Result<(), LibusbError> {
        debug!("Init host");
        if self.ctx.libusb_ctx.is_some() {
            return Ok(());
        }
        unsafe {
            let mut ctx: *mut libusb_context = std::ptr::null_mut();
            let res = libusb_init(&mut ctx);
            if res < 0 {
                return Err(LibusbError::from_raw(res));
            }

            self.ctx.libusb_ctx = Some(ctx);

            let flag = Arc::new(AtomicBool::new(true));
            self.ctx.event_loop_flag = Some(flag.clone());
            let ctx_num = ctx as usize;
            //spawn new thread to handle events (with timeout)
            let handle = thread::spawn(move || {
                let tv = timeval {
                    tv_sec: 0,
                    tv_usec: 20_000, // 20 ms
                };
                while flag.load(Ordering::SeqCst) {
                    let rc = libusb_handle_events_timeout_completed(
                        ctx_num as *mut libusb_context,
                        &tv,
                        std::ptr::null_mut(),
                    );
                    if rc < 0 {
                        error!("Error in libusb_handle_events_timeout: {}", rc);
                        break;
                    }
                }
            });
            self.ctx.event_thread = Some(handle);
            Ok(())
        }
    }

    fn list_devices(
        &mut self,
    ) -> Result<Vec<(Resource<UsbDevice>, DeviceDescriptor, DeviceLocation)>, LibusbError> {
        info!("list_devices called.");
        unsafe {
            let mut list_ptr: *mut *mut libusb_device = std::ptr::null_mut();
            info!("libusb_get_device_list called.");
            let cnt = libusb_get_device_list(
                self.ctx.libusb_ctx.ok_or(LibusbError::NotFound)?,
                &mut list_ptr as *mut _ as *mut _,
            );
            info!("libusb_get_device_list returned count: {}", cnt);
            if cnt < 0 {
                return Err(LibusbError::from_raw(cnt as i32));
            }
            let mut devices: Vec<(Resource<UsbDevice>, DeviceDescriptor, DeviceLocation)> =
                Vec::new();
            for i in 0..cnt {
                let dev = *list_ptr.add(i as usize);
                if dev.is_null() {
                    warn!("Device at index {} is null, skipping.", i);
                    continue;
                }
                info!("Adding device at index {}.", i);
                let resource = self
                    .table
                    .push(UsbDevice { device: dev })
                    .or(Err(LibusbError::Other))?;
                let mut desc =
                    std::mem::MaybeUninit::<libusb1_sys::libusb_device_descriptor>::uninit();
                let res = libusb_get_device_descriptor(dev, desc.as_mut_ptr());
                if res < 0 {
                    warn!(
                        "Failed to get device descriptor for device at index {}: {}",
                        i, res
                    );
                    continue;
                }
                let device_desc = desc.assume_init();
                let vendor_id = device_desc.idVendor;
                let product_id = device_desc.idProduct;
                debug!("vendor_id: {}, product_id: {}", vendor_id, product_id);
                let location = DeviceLocation {
                    bus_number: libusb_get_bus_number(dev),
                    device_address: libusb_get_device_address(dev),
                    port_number: libusb_get_port_number(dev),
                    speed: UsbSpeed::from_raw(libusb_get_device_speed(dev) as u8),
                };

                let device_descriptor = DeviceDescriptor {
                    length: device_desc.bLength,
                    descriptor_type: device_desc.bDescriptorType,
                    usb_version_bcd: device_desc.bcdUSB,
                    device_class: device_desc.bDeviceClass,
                    device_subclass: device_desc.bDeviceSubClass,
                    device_protocol: device_desc.bDeviceProtocol,
                    max_packet_size0: device_desc.bMaxPacketSize0,
                    vendor_id,
                    product_id,
                    device_version_bcd: device_desc.bcdDevice,
                    manufacturer_index: device_desc.iManufacturer,
                    product_index: device_desc.iProduct,
                    serial_number_index: device_desc.iSerialNumber,
                    num_configurations: device_desc.bNumConfigurations,
                };

                devices.push((resource, device_descriptor, location));
            }
            info!("Freeing device list pointer.");
            libusb_free_device_list(list_ptr, 0);
            info!("Returning {} device(s).", devices.len());
            Ok(devices)
        }
    }
}

impl usb::device::HostUsbDevice for WasiUsbCtxView<'_> {
    fn open(
        &mut self,
        self_: Resource<UsbDevice>,
    ) -> Result<Resource<UsbDeviceHandle>, LibusbError> {
        let usb_device = self.table.get(&self_).expect("Failed to get device");
        let device_ptr = usb_device.device;
        unsafe {
            let mut handle_ptr: *mut libusb_device_handle = std::ptr::null_mut();
            let res = libusb_open(device_ptr, &mut handle_ptr);
            if res < 0 {
                return Err(LibusbError::from_raw(res));
            }

            let handle = UsbDeviceHandle { handle: handle_ptr };
            let resource = self.table.push(handle).or(Err(LibusbError::Other))?;
            Ok(resource)
        }
    }

    fn get_configuration_descriptor(
        &mut self,
        self_: Resource<UsbDevice>,
        config_index: u8,
    ) -> Result<ConfigurationDescriptor, LibusbError> {
        let usb_device = self.table.get(&self_).expect("Failed to get device");
        let device_ptr = usb_device.device;
        let mut config_desc: *const libusb_config_descriptor = std::ptr::null();
        unsafe {
            let res = libusb_get_config_descriptor(device_ptr, config_index, &mut config_desc);
            if res < 0 {
                return Err(LibusbError::from_raw(res));
            }
            let descriptor = generate_config_descriptor(&*config_desc);
            libusb_free_config_descriptor(config_desc);
            Ok(descriptor)
        }
    }

    fn get_configuration_descriptor_by_value(
        &mut self,
        self_: Resource<UsbDevice>,
        config_value: u8,
    ) -> Result<ConfigurationDescriptor, LibusbError> {
        let usb_device = self.table.get(&self_).expect("Failed to get device");
        let device_ptr = usb_device.device;
        let mut config_desc: *const libusb_config_descriptor = std::ptr::null();
        unsafe {
            let res =
                libusb_get_config_descriptor_by_value(device_ptr, config_value, &mut config_desc);
            if res < 0 {
                return Err(LibusbError::from_raw(res));
            }
            let descriptor = generate_config_descriptor(&*config_desc);
            // Create the ConfigurationDescriptor from the config_desc
            libusb_free_config_descriptor(config_desc);
            Ok(descriptor)
        }
    }

    fn get_active_configuration_descriptor(
        &mut self,
        self_: Resource<UsbDevice>,
    ) -> Result<ConfigurationDescriptor, LibusbError> {
        let usb_device = self.table.get(&self_).expect("Failed to get device");
        let device_ptr = usb_device.device;
        unsafe {
            let mut config_desc: *const libusb_config_descriptor = std::ptr::null();
            let res = libusb_get_active_config_descriptor(device_ptr, &mut config_desc);
            if res < 0 {
                return Err(LibusbError::from_raw(res));
            }
            let descriptor = generate_config_descriptor(&*config_desc);
            libusb_free_config_descriptor(config_desc);
            Ok(descriptor)
        }
    }

    fn drop(&mut self, rep: Resource<UsbDevice>) -> wasmtime::Result<()> {
        trace!("Drop device");
        if let Ok(device) = self.table.get(&rep) {
            unsafe {
                libusb_unref_device(device.device);
            }
        }
        Ok(())
    }
}

impl usb::device::HostDeviceHandle for WasiUsbCtxView<'_> {
    fn get_configuration(&mut self, self_: Resource<UsbDeviceHandle>) -> Result<u8, LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        unsafe {
            let mut config: i32 = 0;
            let res = libusb_get_configuration(usb_device_handle.handle, &mut config);
            match res {
                0.. => Ok(config as u8),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn set_configuration(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        config: ConfigValue,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        unsafe {
            let config_value = match config {
                ConfigValue::Value(value) => value as i32,
                ConfigValue::Unconfigured => 0,
            };
            let res = libusb_set_configuration(usb_device_handle.handle, config_value);
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn claim_interface(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        unsafe {
            let res = libusb_claim_interface(usb_device_handle.handle, ifac as i32);
            debug!("Claim interface result: {:?}", res);
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn release_interface(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        unsafe {
            let res = libusb_release_interface(usb_device_handle.handle, ifac as i32);
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn set_interface_altsetting(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
        alt_setting: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        unsafe {
            let res = libusb_set_interface_alt_setting(
                usb_device_handle.handle,
                ifac as i32,
                alt_setting as i32,
            );
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn clear_halt(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        endpoint: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        unsafe {
            let res = libusb_clear_halt(usb_device_handle.handle, endpoint);
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn reset_device(&mut self, self_: Resource<UsbDeviceHandle>) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        unsafe {
            let res = libusb_reset_device(usb_device_handle.handle);
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn alloc_streams(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        num_streams: u32,
        endpoints: Vec<u8>,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        let num_endpoints = endpoints.len() as i32;
        let endpoints_ptr = endpoints.as_ptr() as *mut u8;
        unsafe {
            let res = libusb_alloc_streams(
                usb_device_handle.handle,
                num_streams,
                endpoints_ptr,
                num_endpoints,
            );
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn free_streams(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        endpoints: Vec<u8>,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        let num_endpoints = endpoints.len() as i32;
        let endpoints_ptr = endpoints.as_ptr() as *mut u8;
        unsafe {
            let res = libusb_free_streams(usb_device_handle.handle, endpoints_ptr, num_endpoints);
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn kernel_driver_active(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
    ) -> Result<bool, LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        unsafe {
            let res = libusb_kernel_driver_active(usb_device_handle.handle, ifac as i32);
            match res {
                0 => Ok(false),
                1.. => Ok(true),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn detach_kernel_driver(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        unsafe {
            let res = libusb_detach_kernel_driver(usb_device_handle.handle, ifac as i32);
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn attach_kernel_driver(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        ifac: u8,
    ) -> Result<(), LibusbError> {
        let usb_device_handle = self.table.get(&self_).expect("Failed to get device handle");
        unsafe {
            let res = libusb_attach_kernel_driver(usb_device_handle.handle, ifac as i32);
            match res {
                0.. => Ok(()),
                _ => Err(LibusbError::from_raw(res)),
            }
        }
    }

    fn new_transfer(
        &mut self,
        self_: Resource<UsbDeviceHandle>,
        xfer_type: TransferType,
        setup: TransferSetup,
        buf_size: u32,
        opts: TransferOptions,
    ) -> Result<Resource<Transfer>, LibusbError> {
        info!(
            "Starting new_transfer with buf_size: {buf_size} and transfer type: {:?}",
            xfer_type
        );

        let usb_handle = self.table.get(&self_).expect("Failed to get device handle");
        debug!("Retrieved USB device handle: {:?}", usb_handle.handle);

        unsafe {
            let iso_packets = if matches!(xfer_type, TransferType::Isochronous) {
                opts.iso_packets as i32
            } else {
                0
            };
            debug!("Calculated iso_packets: {iso_packets}");

            let transfer_ptr = libusb_alloc_transfer(iso_packets);
            if transfer_ptr.is_null() {
                log::error!(
                    "Failed to allocate USB transfer (libusb_alloc_transfer returned null)"
                );
                return Err(LibusbError::NoMem);
            }
            debug!("Allocated transfer pointer: {:?}", transfer_ptr);

            (*transfer_ptr).dev_handle = usb_handle.handle;
            (*transfer_ptr).endpoint = opts.endpoint;
            (*transfer_ptr).transfer_type = match xfer_type {
                TransferType::Control => LIBUSB_TRANSFER_TYPE_CONTROL,
                TransferType::Bulk => LIBUSB_TRANSFER_TYPE_BULK,
                TransferType::Interrupt => LIBUSB_TRANSFER_TYPE_INTERRUPT,
                TransferType::Isochronous => LIBUSB_TRANSFER_TYPE_ISOCHRONOUS,
            };
            (*transfer_ptr).timeout = opts.timeout_ms;
            debug!(
                "Transfer configured with endpoint: {}, type: {:?}, timeout: {}ms",
                opts.endpoint,
                (*transfer_ptr).transfer_type,
                opts.timeout_ms
            );

            if opts.stream_id != 0 {
                libusb_transfer_set_stream_id(transfer_ptr, opts.stream_id);
                debug!("Stream ID set to: {}", opts.stream_id);
            }

            let total_len: u32 = if (*transfer_ptr).transfer_type == LIBUSB_TRANSFER_TYPE_CONTROL {
                8 + buf_size
                // buf_size
            } else {
                buf_size
            };
            debug!(
                "Calculated total transfer buffer size: {}, based on transfer type: {:?}",
                total_len,
                (*transfer_ptr).transfer_type
            );

            let mut buffer_vec = vec![0u8; total_len as usize];

            if (*transfer_ptr).transfer_type == LIBUSB_TRANSFER_TYPE_CONTROL {
                buffer_vec[0] = setup.bm_request_type;
                buffer_vec[1] = setup.b_request;
                buffer_vec[2] = (setup.w_value & 0xFF) as u8;
                buffer_vec[3] = (setup.w_value >> 8) as u8;
                buffer_vec[4] = (setup.w_index & 0xFF) as u8;
                buffer_vec[5] = (setup.w_index >> 8) as u8;
                buffer_vec[6] = (buf_size & 0xFF) as u8;
                buffer_vec[7] = ((buf_size >> 8) & 0xFF) as u8;

                debug!(
                    "Control transfer setup filled: bm_request_type: {}, b_request: {}, w_value: {}, w_index: {}",
                    setup.bm_request_type, setup.b_request, setup.w_value, setup.w_index
                );
            }

            let buffer_box = buffer_vec.into_boxed_slice();
            (*transfer_ptr).buffer = buffer_box.as_ptr() as *mut u8;
            (*transfer_ptr).length = total_len as i32;
            debug!("Transfer buffer configured with length: {}", total_len);

            if iso_packets > 0 {
                let packet_count = iso_packets as usize;
                let base_len = buf_size / iso_packets as u32;
                let rem = buf_size % iso_packets as u32;

                for i in 0..packet_count {
                    let desc = (*transfer_ptr).iso_packet_desc.as_mut_ptr().add(i);
                    let packet_len = if i == packet_count - 1 {
                        base_len + rem
                    } else {
                        base_len
                    };
                    (*desc).length = packet_len;
                    debug!("Iso packet {} configured with length: {}", i, packet_len);
                }

                (*transfer_ptr).num_iso_packets = iso_packets;
                info!(
                    "Isochronous transfer configured with {} packets",
                    iso_packets
                );
            }

            let transfer_resource = self
                .table
                .push(UsbTransfer {
                    transfer: transfer_ptr,
                    buffer: Some(buffer_box),
                    buf_len: buf_size,
                    completed: Arc::new(AtomicBool::new(false)),
                    receiver: None,
                    control_setup: Option::from(setup),
                })
                .or(Err(LibusbError::Other))?;
            info!("Transfer resource created successfully");

            Ok(transfer_resource)
        }
    }

    fn close(&mut self, self_: Resource<UsbDeviceHandle>) {
        debug!("Close device handle: {}", self_.owned());

        if !self_.owned()
            && let Ok(handle) = self.table.get(&self_)
        {
            unsafe {
                libusb_close(handle.handle);
            }
            self.table
                .delete(self_)
                .expect("something went wrong when closing device-handle resource");
        }
    }

    fn drop(&mut self, rep: Resource<UsbDeviceHandle>) -> wasmtime::Result<()> {
        debug!("Drop device handle: {}", rep.owned());
        self.close(rep);
        Ok(())
    }
}

#[allow(dead_code)]
fn generate_config_descriptor(
    raw_descriptor: &libusb_config_descriptor,
) -> ConfigurationDescriptor {
    unsafe {
        let mut interfaces: Vec<InterfaceDescriptor> = Vec::new();
        for i in 0..raw_descriptor.bNumInterfaces {
            let interface = &*raw_descriptor.interface.wrapping_add(i as usize);
            for j in 0..interface.num_altsetting {
                let mut endpoints: Vec<EndpointDescriptor> = Vec::new();
                let alt_setting = &*interface.altsetting.wrapping_add(j as usize);
                for k in 0..alt_setting.bNumEndpoints {
                    let endpoint = &*alt_setting.endpoint.wrapping_add(k as usize);
                    let endpoint_desc = EndpointDescriptor {
                        length: endpoint.bLength,
                        descriptor_type: endpoint.bDescriptorType,
                        endpoint_address: endpoint.bEndpointAddress,
                        attributes: endpoint.bmAttributes,
                        max_packet_size: endpoint.wMaxPacketSize,
                        interval: endpoint.bInterval,
                        refresh: endpoint.bRefresh,
                        synch_address: endpoint.bSynchAddress,
                    };
                    endpoints.push(endpoint_desc);
                }
                let interface_desc = InterfaceDescriptor {
                    length: alt_setting.bLength,
                    descriptor_type: alt_setting.bDescriptorType,
                    interface_number: alt_setting.bInterfaceNumber,
                    alternate_setting: alt_setting.bAlternateSetting,
                    interface_class: alt_setting.bInterfaceClass,
                    interface_subclass: alt_setting.bInterfaceSubClass,
                    interface_protocol: alt_setting.bInterfaceProtocol,
                    interface_index: alt_setting.iInterface,
                    endpoints,
                };
                interfaces.push(interface_desc);
            }
        }

        ConfigurationDescriptor {
            length: raw_descriptor.bLength,
            descriptor_type: raw_descriptor.bDescriptorType,
            total_length: raw_descriptor.wTotalLength,
            configuration_value: raw_descriptor.bConfigurationValue,
            configuration_index: raw_descriptor.iConfiguration,
            attributes: raw_descriptor.bmAttributes,
            max_power: raw_descriptor.bMaxPower,
            interfaces,
        }
    }
}
