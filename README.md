# wasmtime-wasi-usb

A [Wasmtime](https://github.com/bytecodealliance/wasmtime) host implementation of [`wasi-usb`](https://github.com/WebAssembly/wasi-usb), enabling WebAssembly components to interact with USB devices through the `wasi-usb` interface.

Use this crate to add `wasi-usb` support to any Wasmtime embedder.

## Usage

Add the dependency to your `Cargo.toml`:

```sh
cargo add wasmtime-wasi-usb
```

```toml
[dependencies]
wasmtime-wasi-usb = "44.0.0" # specify desired version
```

### Integrating into a Wasmtime embedder

Implement the `WasiUsbView` trait for your host state and register the interfaces with the linker:

```rust
use wasmtime::component::Linker;
use wasmtime_wasi::ResourceTable;
use wasmtime_wasi_usb::{WasiUsbCtx, WasiUsbCtxView, WasiUsbView};

struct MyState {
    table: ResourceTable,
    usb: WasiUsbCtx,
    // ...
}

impl WasiUsbView for MyState {
    fn usb(&mut self) -> WasiUsbCtxView<'_> {
        WasiUsbCtxView {
            ctx: &mut self.usb,
            table: &mut self.table,
        }
    }
}

fn setup_linker(linker: &mut Linker<MyState>) -> wasmtime::Result<()> {
    wasmtime_wasi_usb::add_to_linker(&mut linker)?;
    // ...
    Ok(())
}
```

Create the USB context when initializing your state. Pass `true` or `false` to enable or disable USB hotplug support:

```rust
let usb_ctx = WasiUsbCtx::new(/* hotplug_enabled: */ true);
```

## License

This project is licensed under the [MIT License](LICENSE).

> This work has been partially supported by the ELASTIC project, which received funding from the Smart Networks and Services Joint Undertaking (SNS JU) under the European Union’s Horizon Europe research and innovation programme under Grant Agreement No 101139067. Views and opinions expressed are however those of the author(s) only and do not necessarily reflect those of the European Union. Neither the European Union nor the granting authority can be held responsible for them.
