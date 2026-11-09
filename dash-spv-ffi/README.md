# Dash SPV FFI

This crate provides C-compatible FFI bindings for the Dash SPV client library.

## Features

- Complete FFI wrapper for DashSpvClient
- Configuration management
- Wallet operations (watch addresses, balance queries, UTXO management)
- Async operation support via callbacks
- Comprehensive error handling
- Memory-safe abstractions

## Building

### Standalone Build

```bash
cargo build --release
```

This will generate:
- Static library: `target/release/libdash_spv_ffi.a`
- Dynamic library: `target/release/libdash_spv_ffi.so` (or `.dylib` on macOS)
- C header: `include/dash_spv_ffi.h`

For the Android and iOS libraries, build with the `mobile` profile (size first, speed on the
header-sync path; `docs/node-program/designs/SB2-notes.md`), for example
`cargo build --profile mobile --target aarch64-linux-android`. The libraries and the generated
header are then under `target/<triple>/mobile/` instead of `target/<triple>/release/`.

## Usage

See `examples/basic_usage.c` for a simple example of using the FFI bindings.

### Basic Example

```c
#include "dash_spv_ffi.h"

// Initialize logging
dash_spv_ffi_init_logging("info", true, NULL, 0);

// Create configuration
FFIClientConfig* config = dash_spv_ffi_config_testnet();
dash_spv_ffi_config_set_data_dir(config, "/path/to/data");

// Create client
FFIDashSpvClient* client = dash_spv_ffi_client_new(config);
if (client == NULL) {
    const char* error = dash_spv_ffi_get_last_error();
    // Handle error
}

// Start the client and begin syncing in the background
if (dash_spv_ffi_client_run(client) != 0) {
    // Handle error
}

// ... use the client ...

// Clean up
dash_spv_ffi_client_destroy(client);
dash_spv_ffi_config_destroy(config);
```

## API Documentation

### Configuration

- `dash_spv_ffi_config_new(network)` - Create new config
- `dash_spv_ffi_config_mainnet()` - Create mainnet config
- `dash_spv_ffi_config_testnet()` - Create testnet config
- `dash_spv_ffi_config_set_data_dir(config, path)` - Set data directory
- `dash_spv_ffi_config_set_validation_mode(config, mode)` - Set validation mode
- `dash_spv_ffi_config_set_max_peers(config, max)` - Set maximum peers
- `dash_spv_ffi_config_add_peer(config, addr)` - Add a peer address. Accepts `"ip:port"`, `[ipv6]:port`, or IP-only (defaults to the network port).
- `dash_spv_ffi_config_destroy(config)` - Free config memory

### Client Operations

- `dash_spv_ffi_client_new(config)` - Create new client
- `dash_spv_ffi_client_run(client)` - Start the client and begin syncing in the background
- `dash_spv_ffi_client_stop(client)` - Stop the client
- `dash_spv_ffi_client_get_sync_progress(client)` - Get sync progress
- `dash_spv_ffi_client_get_stats(client)` - Get client statistics
- `dash_spv_ffi_client_destroy(client)` - Free client memory

### Wallet Operations

- `dash_spv_ffi_client_get_address_balance(client, address)` - Get address balance
- `dash_spv_ffi_client_get_utxos(client)` - Get all UTXOs
- `dash_spv_ffi_client_get_utxos_for_address(client, address)` - Get UTXOs for address

### Error Handling

- `dash_spv_ffi_get_last_error()` - Get last error message
- `dash_spv_ffi_clear_error()` - Clear last error

### Memory Management

All created objects must be explicitly destroyed:
- Config: `dash_spv_ffi_config_destroy()`
- Client: `dash_spv_ffi_client_destroy()`
- Progress: `dash_spv_ffi_sync_progress_destroy()`
- Stats: `dash_spv_ffi_spv_stats_destroy()`
- Balance: `dash_spv_ffi_balance_destroy()`
- Arrays: `dash_spv_ffi_array_destroy()`
- Strings: `dash_spv_ffi_string_destroy()`

## Thread Safety

The FFI bindings are thread-safe. The client uses internal synchronization to ensure safe concurrent access.

The last error is per thread, like `errno`: `dash_spv_ffi_get_last_error()` returns the error of the
last failed call made on the calling thread, and a call on another thread never changes it. The
returned string belongs to the library and is valid only on the calling thread, until that thread
makes another call that sets or clears its error, or exits. Copy it before calling into the library
again or passing it to another thread. The same holds for `FFIResult.error_message`.

In Swift, copy the message (e.g. `String(cString:)`) before any `await`. An async function can resume
on a different thread, which would read that thread's error instead, and a pointer read before the
`await` can be freed while the task is suspended, because the original thread may set or clear its
error, or exit.

## License

CC0-1.0
