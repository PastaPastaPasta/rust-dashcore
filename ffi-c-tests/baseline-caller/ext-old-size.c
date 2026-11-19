/*
 * A caller built against the current header that passes the extension table with the size of a
 * table that ends at on_events_resynced, followed by non-null garbage. The library must accept
 * it and keep no callback past that size, and must reject a size that cuts a field.
 *
 * Usage: ext-old-size <data dir>
 */
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "dash-spv-ffi/dash-spv-ffi.h"

/* FFIErrorCode's values; the header returns them as int32_t. */
#define SUCCESS 0
#define INVALID_ARGUMENT 2

#define OLD_SIZE \
  (offsetof(FFIExtEventCallbacks, on_events_resynced) + sizeof(OnEventsResyncedCallback))

static void on_events_resynced(FFIEventBus bus, uint64_t missed, void *user_data) {
  (void)bus;
  (void)missed;
  (void)user_data;
}

int main(int argc, char **argv) {
  if (argc != 2) {
    fprintf(stderr, "usage: %s <data dir>\n", argv[0]);
    return 2;
  }
  FFIClientConfig *config = dash_spv_ffi_config_new(FFINetwork_Regtest);
  if (config == NULL || dash_spv_ffi_config_set_data_dir(config, argv[1]) != 0) return 1;
  FFIEventCallbacks frozen;
  memset(&frozen, 0, sizeof frozen);
  FFIDashSpvClient *client = dash_spv_ffi_client_new(config, frozen);
  dash_spv_ffi_config_destroy(config);
  if (client == NULL) return 1;

  FFIExtEventCallbacks table;
  memset(&table, 0xAB, sizeof table);
  table.size = OLD_SIZE;
  table.version = 1;
  table.user_data = NULL;
  table.on_events_resynced = on_events_resynced;

  int failed = 0;
  if (dash_spv_ffi_client_set_ext_event_callbacks(client, &table) != SUCCESS) {
    fprintf(stderr, "ext-old-size: the old size was rejected\n");
    failed = 1;
  }
  table.size = OLD_SIZE + 4;
  if (dash_spv_ffi_client_set_ext_event_callbacks(client, &table) != INVALID_ARGUMENT) {
    fprintf(stderr, "ext-old-size: a size that cuts a field was accepted\n");
    failed = 1;
  }
  dash_spv_ffi_client_destroy(client);
  if (!failed) printf("ext-old-size: ok\n");
  return failed;
}
