use log::{debug, error, info, trace, warn};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::oneshot;

use libusb1_sys::constants::*;
use libusb1_sys::{
    libusb_cancel_transfer, libusb_free_transfer, libusb_submit_transfer, libusb_transfer,
};

use wasmtime::component::Resource;

use crate::usb::WasiUsbCtxView;
use crate::usb::bindings::wasi::usb;
use usb::errors::LibusbError;
use usb::transfers::{Transfer, TransferSetup};

#[derive(Debug)]
pub struct UsbTransfer {
    pub transfer: *mut libusb_transfer,
    pub completed: Arc<AtomicBool>,
    pub buffer: Option<Box<[u8]>>,
    pub buf_len: u32,
    pub receiver: Option<oneshot::Receiver<Result<Vec<u8>, LibusbError>>>,
    pub control_setup: Option<TransferSetup>,
    // Per-packet (actual_length, status) results, populated by transfer_callback for ISO.
    // pub iso_packet_results: Arc<Mutex<Option<Vec<(u32, i32)>>>>,
}
unsafe impl Send for UsbTransfer {}

// Context struct for transfer callback
struct TransferContext {
    sender: oneshot::Sender<Result<Vec<u8>, LibusbError>>,
    completed: Arc<AtomicBool>,
    buffer: Box<[u8]>,
}

impl usb::transfers::Host for WasiUsbCtxView<'_> {
    async fn await_transfer(
        &mut self,
        xfer: Resource<UsbTransfer>,
    ) -> Result<Vec<u8>, LibusbError> {
        info!("Awaiting transfer");
        let usb_transfer = self.table.get_mut(&xfer).expect("Failed to get transfer");

        if usb_transfer.receiver.is_none() {
            error!("Transfer receiver not set");
            return Err(LibusbError::NotFound);
        }

        let receiver = usb_transfer.receiver.take().ok_or(LibusbError::NotFound)?;
        info!("Transfer receiver set");

        let result = match receiver.await {
            Ok(result) => {
                info!("Transfer result: {:?}", result);
                result
            }
            Err(_) => Err(LibusbError::Interrupted),
        };

        self.table.delete(xfer).ok();
        result
    }
}

impl usb::transfers::HostTransfer for WasiUsbCtxView<'_> {
    fn submit_transfer(
        &mut self,
        self_: Resource<Transfer>,
        data: Vec<u8>,
    ) -> Result<(), LibusbError> {
        debug!("Submit transfer");
        let usb_transfer = self.table.get_mut(&self_).expect("Failed to get transfer");
        debug!("Transfer: {:?}", usb_transfer);
        let transfer_ptr = usb_transfer.transfer;
        if usb_transfer.completed.load(Ordering::SeqCst) {
            warn!("Transfer already completed");
            return Err(LibusbError::Busy);
        }

        unsafe {
            let transfer_type = (*transfer_ptr).transfer_type;
            debug!("Transfer type: {:?}", transfer_type);

            if transfer_type == LIBUSB_TRANSFER_TYPE_CONTROL {
                let setup_buf = (*transfer_ptr).buffer;
                if !setup_buf.is_null() {
                    let bm_request_type = usb_transfer.control_setup.unwrap().bm_request_type;
                    let direction_in = bm_request_type & 0x80 != 0;
                    if direction_in {
                        // control transfer IN
                        debug!("Control transfer IN");
                    } else {
                        debug!("Control transfer OUT");
                        // control transfer out
                        if data.len() as u32 != usb_transfer.buf_len {
                            error!(
                                "Invalid data length for control transfer OUT: {}, expected {}",
                                data.len(),
                                usb_transfer.buf_len
                            );
                            return Err(LibusbError::InvalidParam);
                        }
                        let buf_ptr = (*transfer_ptr).buffer;
                        if !buf_ptr.is_null() {
                            debug!("Copying data to control transfer OUT buffer");
                            std::ptr::copy_nonoverlapping(
                                data.as_ptr(),
                                setup_buf.add(8),
                                data.len(),
                            );
                        }
                    }
                }
            } else if (*transfer_ptr).endpoint & 0x80 != 0 {
                // IN transfer
                info!("IN transfer");
            } else {
                info!("OUT transfer");
                // OUT transfer
                if data.len() as u32 != usb_transfer.buf_len {
                    error!(
                        "Invalid data length for OUT transfer: {}, expected {}",
                        data.len(),
                        usb_transfer.buf_len
                    );
                    return Err(LibusbError::InvalidParam);
                }
                let buf_ptr = (*transfer_ptr).buffer;
                if !buf_ptr.is_null() {
                    debug!("Copying data to OUT transfer buffer");
                    std::ptr::copy_nonoverlapping(data.as_ptr(), buf_ptr, data.len());
                }
            }

            debug!("creating transfer context");

            let (sender, receiver) = oneshot::channel();

            let buffer_box = usb_transfer.buffer.take().expect("buffer not allocated");
            let ctx = Box::new(TransferContext {
                sender,
                completed: usb_transfer.completed.clone(),
                buffer: buffer_box,
            });

            (*transfer_ptr).user_data = Box::into_raw(ctx) as *mut _;
            (*transfer_ptr).callback = transfer_callback;

            debug!("submitting transfer: {:?}", transfer_ptr);
            let submit_result = libusb_submit_transfer(transfer_ptr);
            if submit_result < 0 {
                error!(
                    "Failed to submit transfer: {}",
                    LibusbError::from_raw(submit_result)
                );
                let _ = Box::from_raw((*transfer_ptr).user_data as *mut TransferContext);
                (*transfer_ptr).callback = empty_callback;
                (*transfer_ptr).user_data = std::ptr::null_mut();
                return Err(LibusbError::from_raw(submit_result));
            } else {
                debug!("transfer submitted");
                let transfer_mut = self.table.get_mut(&self_).expect("Failed to get transfer");
                transfer_mut.receiver = Some(receiver);
            }
        }
        Ok(())
    }

    fn cancel_transfer(&mut self, self_: Resource<Transfer>) -> Result<(), LibusbError> {
        let usb_transfer = self.table.get(&self_).expect("Failed to get transfer");
        let transfer_ptr = usb_transfer.transfer;
        unsafe {
            if !usb_transfer.completed.load(Ordering::SeqCst) {
                let res = libusb_cancel_transfer(transfer_ptr);
                if res < 0 {
                    return Err(LibusbError::from_raw(res));
                }
            }
        }
        Ok(())
    }

    fn drop(&mut self, rep: Resource<Transfer>) -> wasmtime::Result<()> {
        trace!("Drop transfer");
        if let Ok(transfer) = self.table.get(&rep) {
            unsafe {
                if !transfer.completed.load(Ordering::SeqCst) {
                    let _ = libusb_cancel_transfer(transfer.transfer);
                } else {
                    // If the transfer is already completed, we can safely drop it
                    // without calling `libusb_cancel_transfer`.
                }
            }
        }
        Ok(())
    }
}

#[allow(dead_code)]
extern "system" fn transfer_callback(transfer: *mut libusb_transfer) {
    unsafe {
        // Reconstruct the context
        let ctx_ptr = (*transfer).user_data as *mut TransferContext;
        let ctx = Box::from_raw(ctx_ptr);
        // Determine transfer status and prepare result
        let status = (*transfer).status;
        let result: Result<Vec<u8>, LibusbError> = if status == LIBUSB_TRANSFER_COMPLETED {
            // Transfer completed successfully
            let mut data_vec = Vec::new();
            if (*transfer).num_iso_packets > 0 {
                // Isochronous transfer: combine data from all packets
                let num_packets = (*transfer).num_iso_packets as usize;
                let mut total_len: usize = 0;
                for i in 0..num_packets {
                    let desc = (*transfer).iso_packet_desc.as_ptr().add(i);
                    total_len += (*desc).actual_length as usize;
                }
                let buf_ptr = (*transfer).buffer;
                if !buf_ptr.is_null() && total_len > 0 {
                    let data_slice = std::slice::from_raw_parts(buf_ptr, total_len);
                    data_vec = data_slice.to_vec();
                }
            } else if (*transfer).transfer_type == LIBUSB_TRANSFER_TYPE_CONTROL {
                // Control transfer
                // For control IN (device-to-host): skip setup packet (first 8 bytes)
                let actual_len = (*transfer).actual_length as usize;
                debug!(
                    "Control transfer completed with actual length: {}",
                    actual_len
                );

                // Extract request type from the setup packet
                let buf_ptr = (*transfer).buffer;
                let bm_request_type = if !buf_ptr.is_null() { *buf_ptr } else { 0 };
                let is_device_to_host = (bm_request_type & 0x80) != 0;

                if is_device_to_host && actual_len > 0 {
                    // For IN transfers, return the data after the setup packet
                    if !buf_ptr.is_null() {
                        // Get the data portion (skipping 8-byte setup)
                        let data_slice = std::slice::from_raw_parts(buf_ptr.add(8), actual_len);
                        data_vec = data_slice.to_vec();
                        debug!("Control IN transfer data: {:?}", data_vec);
                    }
                } else {
                    // For OUT transfers, no data to return
                    data_vec = Vec::new();
                }
            } else {
                // Bulk/Interrupt transfer
                let actual_len = (*transfer).actual_length as usize;
                if (*transfer).endpoint & 0x80 != 0 {
                    // IN transfer: copy received data
                    if actual_len > 0 {
                        let buf_ptr = (*transfer).buffer;
                        if !buf_ptr.is_null() {
                            let data_slice = std::slice::from_raw_parts(buf_ptr, actual_len);
                            data_vec = data_slice.to_vec();
                        }
                    }
                } else {
                    // OUT transfer: no data to return
                    data_vec = Vec::new();
                }
            }
            Ok(data_vec)
        } else {
            // Transfer did not complete successfully, map status to LibusbError
            let err = match status {
                LIBUSB_TRANSFER_TIMED_OUT => LibusbError::Timeout,
                LIBUSB_TRANSFER_CANCELLED => LibusbError::Interrupted,
                LIBUSB_TRANSFER_STALL => LibusbError::Pipe,
                LIBUSB_TRANSFER_NO_DEVICE => LibusbError::NoDevice,
                LIBUSB_TRANSFER_OVERFLOW => LibusbError::Overflow,
                LIBUSB_TRANSFER_ERROR => LibusbError::Io,
                _ => LibusbError::Other,
            };
            Err(err)
        };
        // Mark as completed
        ctx.completed.store(true, Ordering::SeqCst);
        // Send result (if receiver still exists)
        let _ = ctx.sender.send(result);
        // Free the libusb transfer struct
        libusb_free_transfer(transfer);
        // Box::from_raw has taken ownership of ctx, dropping it here will free buffer
        // (Buffer is inside ctx.buffer as Box<[u8]> and will be dropped automatically)
    }
}

#[allow(dead_code)]
extern "system" fn empty_callback(_transfer: *mut libusb_transfer) {}
