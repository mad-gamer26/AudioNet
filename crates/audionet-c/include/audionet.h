/* AudioNet's engine for C and C++ programs (crates/audionet-c).
 *
 * Strings in: UTF-8, NUL-terminated. Strings out: written into the caller's
 * buffer (NUL-terminated); the return value is the length, or minus the
 * size needed when the buffer is too small (nothing written). Objects are
 * numbered handles; 0 is "none". After a failure, an_last_error says why in
 * words (per thread). Events are JSON text, read with an_client_poll; there
 * are no callbacks. Only sign-in and sign-out wait (for the network).
 */
#ifndef AUDIONET_H
#define AUDIONET_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

int32_t an_version(char* out, int32_t cap);
int32_t an_last_error(char* out, int32_t cap);

/* Visitor sign-in: the password is used once; the session token comes out. */
int32_t an_visitor_sign_in(const char* server, const char* username, const char* password, char* out_token, int32_t cap);
int32_t an_visitor_sign_out(const char* server, const char* token);

/* A connection to the account (starts at once). */
uint32_t an_client_create(const char* server, const char* username, const char* token, const char* app_name);
void an_client_destroy(uint32_t client);
/* Next event as JSON: {"type": "connected" | "disconnected" | "status" |
 * "devices" | "device" | "stream" | "stream_ended" | "diagnostics" |
 * "server_problem" | "stopped", ...}. 0 when none. */
int32_t an_client_poll(uint32_t client, char* out, int32_t cap);
void an_client_refresh(uint32_t client);
int32_t an_client_listen(uint32_t client, const char* node_id, const char* source_id, const char* output_id, char* out_session, int32_t cap);
int32_t an_client_send(uint32_t client, const char* node_id, const char* source_id, const char* output_id, char* out_session, int32_t cap);
void an_client_stop(uint32_t client, const char* session_id);
void an_client_set_volume(uint32_t client, const char* session_id, float volume, int32_t muted);

/* This computer's sources and outputs as JSON. */
int32_t an_local_audio(char* out, int32_t cap);

/* Audio to the program: listen with the tap's id as the output, then read. */
uint32_t an_tap_create(const char* name, uint32_t sample_rate, uint32_t channels);
void an_tap_destroy(uint32_t tap);
int32_t an_tap_id(uint32_t tap, char* out, int32_t cap);
int32_t an_tap_read(uint32_t tap, float* out, uint32_t frames);
int32_t an_tap_playing(uint32_t tap);

/* Audio from the program: send with the source's id, then write. */
uint32_t an_source_create(const char* name, uint32_t sample_rate, uint32_t channels);
void an_source_destroy(uint32_t source);
int32_t an_source_id(uint32_t source, char* out, int32_t cap);
int32_t an_source_write(uint32_t source, const float* samples, uint32_t frames);
int32_t an_source_sending(uint32_t source);

#ifdef __cplusplus
}
#endif

#endif
