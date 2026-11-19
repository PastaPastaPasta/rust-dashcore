/*
 * A C caller built only against the baseline headers. It syncs from one regtest peer, follows
 * a reorg its driver makes, stops and destroys the client, and checks the frozen callbacks.
 *
 * Usage: caller <data dir> <peer host:port> <initial height>
 * Protocol on stdout/stdin with the driver:
 *   caller prints "SYNCED <header tip>" once the initial sync completes;
 *   driver writes "REORG <height>" after it made the peer reorg to <height>;
 *   caller prints "DONE" after stop and destroy, then one "COUNT <name> <n>" line per counter.
 */
#define _POSIX_C_SOURCE 200809L

#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "dash-spv-ffi/dash-spv-ffi.h"

#define TIMEOUT_SECONDS 300

typedef struct State {
  atomic_uint sync_start;
  atomic_uint headers_stored;
  atomic_uint header_tip;
  atomic_uint filter_headers_stored;
  atomic_uint filter_header_tip;
  atomic_uint sync_complete;
  atomic_uint sync_complete_tip;
  atomic_uint peer_connected;
  atomic_uint peers_updated;
  atomic_uint best_height;
  atomic_uint progress;
  atomic_uint client_error;
  atomic_uint bad_value;
} State;

static void on_sync_start(FFIManagerId manager_id, void *user_data) {
  State *s = user_data;
  if (manager_id > FFIManagerId_Mempool) atomic_fetch_add(&s->bad_value, 1);
  atomic_fetch_add(&s->sync_start, 1);
}

static void on_block_headers_stored(uint32_t tip_height, void *user_data) {
  State *s = user_data;
  atomic_store(&s->header_tip, tip_height);
  atomic_fetch_add(&s->headers_stored, 1);
}

static void on_filter_headers_stored(uint32_t start_height, uint32_t end_height,
                                     uint32_t tip_height, void *user_data) {
  State *s = user_data;
  if (start_height > end_height || end_height > tip_height) atomic_fetch_add(&s->bad_value, 1);
  atomic_store(&s->filter_header_tip, tip_height);
  atomic_fetch_add(&s->filter_headers_stored, 1);
}

static void on_sync_complete(uint32_t header_tip, uint32_t cycle, void *user_data) {
  State *s = user_data;
  (void)cycle;
  atomic_store(&s->sync_complete_tip, header_tip);
  atomic_fetch_add(&s->sync_complete, 1);
}

static void on_peer_connected(const char *address, void *user_data) {
  State *s = user_data;
  if (address == NULL || strchr(address, ':') == NULL) atomic_fetch_add(&s->bad_value, 1);
  atomic_fetch_add(&s->peer_connected, 1);
}

static void on_peers_updated(uint32_t connected_count, uint32_t best_height, void *user_data) {
  State *s = user_data;
  if (connected_count > 1) atomic_fetch_add(&s->bad_value, 1);
  atomic_store(&s->best_height, best_height);
  atomic_fetch_add(&s->peers_updated, 1);
}

static void on_progress(const FFISyncProgress *progress, void *user_data) {
  State *s = user_data;
  if (progress == NULL || progress->percentage < 0.0 || progress->percentage > 1.0)
    atomic_fetch_add(&s->bad_value, 1);
  atomic_fetch_add(&s->progress, 1);
}

static void on_error(const char *error, void *user_data) {
  State *s = user_data;
  fprintf(stderr, "on_error: %s\n", error ? error : "(null)");
  atomic_fetch_add(&s->client_error, 1);
}

static bool wait_until(atomic_uint *value, unsigned at_least, atomic_uint *gate) {
  time_t start = time(NULL);
  struct timespec pause = {0, 50 * 1000 * 1000};
  while (atomic_load(value) < at_least || (gate != NULL && atomic_load(gate) == 0)) {
    if (time(NULL) - start > TIMEOUT_SECONDS) return false;
    nanosleep(&pause, NULL);
  }
  return true;
}

static int fail(const char *what) {
  const char *last = dash_spv_ffi_get_last_error();
  fprintf(stderr, "caller: %s (last error: %s)\n", what, last ? last : "none");
  return 1;
}

int main(int argc, char **argv) {
  if (argc != 4) {
    fprintf(stderr, "usage: %s <data dir> <peer> <initial height>\n", argv[0]);
    return 2;
  }
  unsigned initial_height = (unsigned)strtoul(argv[3], NULL, 10);
  static State state;

  FFIClientConfig *config = dash_spv_ffi_config_new(FFINetwork_Regtest);
  if (config == NULL) return fail("config_new");
  if (dash_spv_ffi_config_set_data_dir(config, argv[1]) != 0) return fail("set_data_dir");
  if (dash_spv_ffi_config_set_masternode_sync_enabled(config, false) != 0)
    return fail("set_masternode_sync_enabled");
  if (dash_spv_ffi_config_add_peer(config, argv[2]) != 0) return fail("add_peer");
  if (dash_spv_ffi_config_set_restrict_to_configured_peers(config, true) != 0)
    return fail("set_restrict_to_configured_peers");

  FFIEventCallbacks callbacks;
  memset(&callbacks, 0, sizeof callbacks);
  callbacks.sync.on_sync_start = on_sync_start;
  callbacks.sync.on_block_headers_stored = on_block_headers_stored;
  callbacks.sync.on_filter_headers_stored = on_filter_headers_stored;
  callbacks.sync.on_sync_complete = on_sync_complete;
  callbacks.sync.user_data = &state;
  callbacks.network.on_peer_connected = on_peer_connected;
  callbacks.network.on_peers_updated = on_peers_updated;
  callbacks.network.user_data = &state;
  callbacks.progress.on_progress = on_progress;
  callbacks.progress.user_data = &state;
  callbacks.error.on_error = on_error;
  callbacks.error.user_data = &state;

  FFIDashSpvClient *client = dash_spv_ffi_client_new(config, callbacks);
  if (client == NULL) return fail("client_new");
  if (dash_spv_ffi_client_run(client) != 0) return fail("client_run");

  if (!wait_until(&state.header_tip, initial_height, &state.sync_complete))
    return fail("initial sync not complete in time");
  printf("SYNCED %u\n", atomic_load(&state.header_tip));
  fflush(stdout);

  unsigned reorg_height = 0;
  if (scanf(" REORG %u", &reorg_height) != 1) return fail("no REORG line from the driver");
  if (!wait_until(&state.header_tip, reorg_height, NULL))
    return fail("reorg headers not stored in time");
  if (!wait_until(&state.filter_header_tip, reorg_height, NULL))
    return fail("reorg filter headers not stored in time");

  if (dash_spv_ffi_client_stop(client) != 0) return fail("client_stop");
  dash_spv_ffi_client_destroy(client);
  dash_spv_ffi_config_destroy(config);
  printf("DONE\n");

  printf("COUNT sync_start %u\n", atomic_load(&state.sync_start));
  printf("COUNT headers_stored %u\n", atomic_load(&state.headers_stored));
  printf("COUNT header_tip %u\n", atomic_load(&state.header_tip));
  printf("COUNT filter_headers_stored %u\n", atomic_load(&state.filter_headers_stored));
  printf("COUNT filter_header_tip %u\n", atomic_load(&state.filter_header_tip));
  printf("COUNT sync_complete %u\n", atomic_load(&state.sync_complete));
  printf("COUNT sync_complete_tip %u\n", atomic_load(&state.sync_complete_tip));
  printf("COUNT peer_connected %u\n", atomic_load(&state.peer_connected));
  printf("COUNT peers_updated %u\n", atomic_load(&state.peers_updated));
  printf("COUNT best_height %u\n", atomic_load(&state.best_height));
  printf("COUNT progress %u\n", atomic_load(&state.progress));
  printf("COUNT client_error %u\n", atomic_load(&state.client_error));
  printf("COUNT bad_value %u\n", atomic_load(&state.bad_value));
  return 0;
}
