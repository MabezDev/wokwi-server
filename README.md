
# Wokwi server

A CLI tool for launching a wokwi instance for your project.

[![asciicast](https://asciinema.org/a/496018.svg)](https://asciinema.org/a/496018)

## Installation

Download the prebuilt executables for you platform from [the releases pages](https://github.com/MabezDev/wokwi-server/releases). Alternatively, if you have Rust installed you can install it via cargo.

```rust
cargo install wokwi-server --git https://github.com/MabezDev/wokwi-server --locked
```

## Usage

Only two arguments are required, the target, specified with `--chip` and the path to your application elf file. Example running the esp-idf blink example on Wokwi:

```sh
idf.py build # build the application
wokwi-server --chip esp32 build/blink.elf # running example opened in the browser!
```

### Simulating your binary on a custom Wokwi project

You can use the ID of a Wokwi project to simulate your resulting binary on it:
```sh
wokwi-server --chip <chip> --id <projectId> build/blink.elf
```

The ID of a Wokwi project can be found in the URL. E.g., the ID of
[ESP32 Rust Blinky](https://wokwi.com/projects/345932416223806035) is `345932416223806035`.

### As a cargo runner

Inside `.cargo/config.toml`, add a `runner` section to your `target` key ([cargo reference](https://doc.rust-lang.org/cargo/reference/config.html)). Example for the esp32:

```toml
runner = "wokwi-server --chip esp32"
```

Once configured, it's possible to launch and run your application in the Wokwi simulator by running `cargo run`.

## GDB support

Wokwi exposes a GDB stub which this tool exposes via a TCP connection, see the following vscode configuration as a reference.

```jsonc
{
    "type": "gdb",
    "request": "attach",
    "name": "VsCode: Wokwi Debug",
    // change this!
    "executable": "${workspaceFolder}/target/xtensa-esp32-espidf/debug/esp-fs-tests",
    "target": "127.0.0.1:9333",
    "remote": true,
    // change this!
    "gdbpath": "xtensa-esp32-elf/bin/xtensa-esp32-elf-gdb",
    "cwd": "${workspaceRoot}",
    "stopAtConnect": true,
    "valuesFormatting": "parseText"
}
```
