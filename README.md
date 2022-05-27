
# Wokwi server

A CLI tool for launching a wokwi instance for your project.

[![asciicast](https://asciinema.org/a/496018.svg)](https://asciinema.org/a/496018)

## Installation

Download the prebuilt executables for you platform from [the releases pages](https://github.com/MabezDev/wokwi-server/releases). Alternatively, if you have Rust installed you can install it via cargo.

```rust
cargo install wokwi-server --git https://github.com/MabezDev/wokwi-server
```

## Usage

Only two arguments are required, the target, specified with `--target` and the path to your application elf file. Example running the esp-idf blink example on Wokwi:

```
$ idf.py build # build the application
$ wokwi-server --chip esp32 build/blink.elf # running example opened in the browser!
```

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
