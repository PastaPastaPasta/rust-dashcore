#include <stddef.h>

#include "dash-spv-ffi/dash-spv-ffi.h"

static void on_events_resynced(FFIEventBus bus, uint64_t missed, void *user_data) {
  (void)bus;
  (void)missed;
  (void)user_data;
}

static void on_chain_reorganized(uint32_t fork_height, uint32_t disconnected, uint32_t connected,
                                 void *user_data) {
  (void)fork_height;
  (void)disconnected;
  (void)connected;
  (void)user_data;
}

static void on_fail_stop(FFIFailStopSource source, const char *message, void *user_data) {
  (void)source;
  (void)message;
  (void)user_data;
}

static void on_subscriber_stalled(const char *const *commands, uint32_t count, void *user_data) {
  (void)commands;
  (void)count;
  (void)user_data;
}

static void on_pinned_peer_failing(const char *addr, uint32_t failures, const char *last,
                                   void *user_data) {
  (void)addr;
  (void)failures;
  (void)last;
  (void)user_data;
}

int set_all(FFIDashSpvClient *client) {
  FFIExtEventCallbacks callbacks = {
      .size = sizeof(FFIExtEventCallbacks),
      .version = FFI_EXT_EVENT_CALLBACKS_VERSION,
      .user_data = NULL,
      .on_events_resynced = on_events_resynced,
      .on_chain_reorganized = on_chain_reorganized,
      .on_fail_stop = on_fail_stop,
      .on_subscriber_stalled = on_subscriber_stalled,
      .on_pinned_peer_failing = on_pinned_peer_failing,
  };
  FFIEventBus bus = FFIEventBus_Wallet;
  FFIFailStopSource source = FFIFailStopSource_Other;
  (void)bus;
  (void)source;
  return dash_spv_ffi_client_set_ext_event_callbacks(client, &callbacks);
}

int main() { return 0; }
