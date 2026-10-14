// AudioNet for NVGT: a plugin that gives NVGT scripts AudioNet's engine
// (crates/audionet-c, linked in statically: one DLL to ship).
//
// Scripts use the friendlier classes in nvgt/include/audionet.nvgt; this
// file only registers the global functions they are built on. Everything
// runs on the script's thread and never waits for audio: only signing in
// and out wait (for the network). Events are polled, never called back.
//
// Audio: a tap's frames are read into a float[] (resized, then filled in
// place); an app source takes a float[]. Scripts play tap audio through an
// NVGT audio_ring_buffer and sound (3D, effects, mixers), topping it up to
// a short depth each loop: NVGT's playback sets the pace, and AudioNet's
// drift and depth control follow it.

#include "nvgt_plugin.h"
#include <scriptarray.h>
#include <string>
#include <vector>

#include "../../crates/audionet-c/include/audionet.h"

namespace {

// A string from a call that writes into a buffer (see audionet.h): grows
// the buffer when asked to, once.
template <typename F>
std::string text(F call) {
	std::vector<char> buf(4096);
	int n = call(buf.data(), (int)buf.size());
	if (n < 0) {
		buf.resize((size_t)(-n));
		n = call(buf.data(), (int)buf.size());
	}
	return n > 0 ? std::string(buf.data(), (size_t)n) : std::string();
}

std::string version() {
	return text([](char* out, int cap) { return an_version(out, cap); });
}

std::string last_error() {
	return text([](char* out, int cap) { return an_last_error(out, cap); });
}

std::string sign_in(const std::string& server, const std::string& user, const std::string& password) {
	return text([&](char* out, int cap) { return an_visitor_sign_in(server.c_str(), user.c_str(), password.c_str(), out, cap); });
}

bool sign_out(const std::string& server, const std::string& token) {
	return an_visitor_sign_out(server.c_str(), token.c_str()) != 0;
}

unsigned int client_create(const std::string& server, const std::string& user, const std::string& token, const std::string& app) {
	return an_client_create(server.c_str(), user.c_str(), token.c_str(), app.c_str());
}

void client_destroy(unsigned int c) { an_client_destroy(c); }

std::string client_poll(unsigned int c) {
	return text([c](char* out, int cap) { return an_client_poll(c, out, cap); });
}

void client_refresh(unsigned int c) { an_client_refresh(c); }

std::string client_listen(unsigned int c, const std::string& node, const std::string& source, const std::string& output) {
	return text([&](char* out, int cap) { return an_client_listen(c, node.c_str(), source.c_str(), output.c_str(), out, cap); });
}

std::string client_send(unsigned int c, const std::string& node, const std::string& source, const std::string& output) {
	return text([&](char* out, int cap) { return an_client_send(c, node.c_str(), source.c_str(), output.c_str(), out, cap); });
}

void client_stop(unsigned int c, const std::string& session) { an_client_stop(c, session.c_str()); }

void client_set_volume(unsigned int c, const std::string& session, float volume, bool muted) {
	an_client_set_volume(c, session.c_str(), volume, muted ? 1 : 0);
}

std::string local_audio() {
	return text([](char* out, int cap) { return an_local_audio(out, cap); });
}

unsigned int tap_create(const std::string& name, unsigned int rate, unsigned int channels) {
	return an_tap_create(name.c_str(), rate, channels);
}

void tap_destroy(unsigned int t) { an_tap_destroy(t); }

std::string tap_id(unsigned int t) {
	return text([t](char* out, int cap) { return an_tap_id(t, out, cap); });
}

bool tap_playing(unsigned int t) { return an_tap_playing(t) != 0; }

// Reads `frames` frames into `out` (resized to frames times channels).
// Returns the frames read.
unsigned int tap_read(unsigned int t, CScriptArray* out, unsigned int frames, unsigned int channels) {
	if (!out || !channels) return 0;
	out->Resize(frames * channels);
	if (!frames) return 0;
	int n = an_tap_read(t, (float*)out->GetBuffer(), frames);
	return n > 0 ? (unsigned int)n : 0;
}

unsigned int source_create(const std::string& name, unsigned int rate, unsigned int channels) {
	return an_source_create(name.c_str(), rate, channels);
}

void source_destroy(unsigned int s) { an_source_destroy(s); }

std::string source_id(unsigned int s) {
	return text([s](char* out, int cap) { return an_source_id(s, out, cap); });
}

bool source_sending(unsigned int s) { return an_source_sending(s) != 0; }

// Writes the samples (interleaved, a whole number of frames). Returns the
// frames taken.
unsigned int source_write(unsigned int s, CScriptArray* samples, unsigned int channels) {
	if (!samples || !channels || samples->GetSize() < channels) return 0;
	int n = an_source_write(s, (const float*)samples->GetBuffer(), samples->GetSize() / channels);
	return n > 0 ? (unsigned int)n : 0;
}

} // namespace

plugin_main(nvgt_plugin_shared* shared) {
	if (!prepare_plugin(shared)) return false;
	asIScriptEngine* e = shared->script_engine;
	// The array code compiled into this plugin took its allocator before
	// NVGT handed its functions over: give it NVGT's own, so arrays are
	// allocated and freed alike on both sides.
	CScriptArray::SetMemoryFunctions(asAllocMem, asFreeMem);
	// Built against AngelScript ANGELSCRIPT_VERSION_STRING; another
	// version's engine could not be called safely.
	if (std::string(asGetLibraryVersion()) != ANGELSCRIPT_VERSION_STRING) return false;
	int r = 0;
	auto reg = [&](const char* decl, const asSFuncPtr& f) {
		if (r >= 0) r = e->RegisterGlobalFunction(decl, f, asCALL_CDECL);
	};
	reg("string audionet_version()", asFUNCTION(version));
	reg("string audionet_last_error()", asFUNCTION(last_error));
	reg("string audionet_visitor_sign_in(const string&in server, const string&in username, const string&in password)", asFUNCTION(sign_in));
	reg("bool audionet_visitor_sign_out(const string&in server, const string&in token)", asFUNCTION(sign_out));
	reg("uint audionet_client_create(const string&in server, const string&in username, const string&in token, const string&in app_name)", asFUNCTION(client_create));
	reg("void audionet_client_destroy(uint client)", asFUNCTION(client_destroy));
	reg("string audionet_client_poll(uint client)", asFUNCTION(client_poll));
	reg("void audionet_client_refresh(uint client)", asFUNCTION(client_refresh));
	reg("string audionet_client_listen(uint client, const string&in node_id, const string&in source_id, const string&in output_id)", asFUNCTION(client_listen));
	reg("string audionet_client_send(uint client, const string&in node_id, const string&in source_id, const string&in output_id)", asFUNCTION(client_send));
	reg("void audionet_client_stop(uint client, const string&in session_id)", asFUNCTION(client_stop));
	reg("void audionet_client_set_volume(uint client, const string&in session_id, float volume, bool muted)", asFUNCTION(client_set_volume));
	reg("string audionet_local_audio()", asFUNCTION(local_audio));
	reg("uint audionet_tap_create(const string&in name, uint sample_rate, uint channels)", asFUNCTION(tap_create));
	reg("void audionet_tap_destroy(uint tap)", asFUNCTION(tap_destroy));
	reg("string audionet_tap_id(uint tap)", asFUNCTION(tap_id));
	reg("bool audionet_tap_playing(uint tap)", asFUNCTION(tap_playing));
	reg("uint audionet_tap_read(uint tap, float[]@ buffer, uint frames, uint channels)", asFUNCTION(tap_read));
	reg("uint audionet_source_create(const string&in name, uint sample_rate, uint channels)", asFUNCTION(source_create));
	reg("void audionet_source_destroy(uint source)", asFUNCTION(source_destroy));
	reg("string audionet_source_id(uint source)", asFUNCTION(source_id));
	reg("bool audionet_source_sending(uint source)", asFUNCTION(source_sending));
	reg("uint audionet_source_write(uint source, const float[]@ samples, uint channels)", asFUNCTION(source_write));
	return r >= 0;
}
