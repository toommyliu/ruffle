#ifndef RUFFLE_APPLE_H
#define RUFFLE_APPLE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/// Opaque handle to a Ruffle Player instance and its async task queue.
typedef struct RufflePlayerHandle RufflePlayerHandle;

/// Optional callback invoked when the SWF calls `ExternalInterface.call(name, ...args)`.
/// `args_json_utf8` is a JSON array of the arguments passed from ActionScript.
typedef void (*RuffleExternalCallCallback)(
    void* user_data,
    const char* name_utf8,
    const char* args_json_utf8
);

/// Optional callback invoked when Ruffle requests opening or closing the virtual keyboard
/// (for example, when an editable `TextField` gains or loses focus).
typedef void (*RuffleVirtualKeyboardCallback)(
    void* user_data,
    bool is_open
);

/// Optional callback invoked when the SWF calls `navigateToURL(request, target)` for an external URL.
typedef void (*RuffleNavigateUrlCallback)(
    void* user_data,
    const char* url_utf8
);

/// Creates a new Ruffle Player instance.
///
/// - If `metal_layer_ptr` is non-null, renders directly into that `CAMetalLayer` via Metal.
/// - If `metal_layer_ptr` is null, creates an offscreen Metal render target (useful for headless testing).
/// - `storage_dir_utf8` is an optional path for storing `.sol` Local Shared Objects.
RufflePlayerHandle* ruffle_player_create(
    void* metal_layer_ptr,
    uint32_t width,
    uint32_t height,
    double scale_factor,
    const char* storage_dir_utf8
);

/// Registers a callback for `ExternalInterface.call(...)` invocations from ActionScript.
void ruffle_player_set_external_call_callback(
    RufflePlayerHandle* handle,
    RuffleExternalCallCallback callback,
    void* user_data
);

/// Registers a callback invoked when Ruffle requests opening (`true`) or closing (`false`) the virtual keyboard.
void ruffle_player_set_virtual_keyboard_callback(
    RufflePlayerHandle* handle,
    RuffleVirtualKeyboardCallback callback,
    void* user_data
);

/// Registers a callback invoked when the SWF requests opening an external website via `navigateToURL`.
void ruffle_player_set_navigate_url_callback(
    RufflePlayerHandle* handle,
    RuffleNavigateUrlCallback callback,
    void* user_data
);

/// Loads a local `.swf` file from `file_path_utf8`.
/// Any relative or external `.swf` files loaded at runtime by this movie (e.g. `loader.swf`)
/// are resolved relative to `file_path_utf8`'s parent directory or fetched via HTTPS.
bool ruffle_player_load_swf_path(
    RufflePlayerHandle* handle,
    const char* file_path_utf8
);

/// Loads an in-memory `.swf` buffer, using `base_url_utf8` (e.g. `"file:///path/to/dir/"` or
/// `"https://example.com/game/"`) to resolve any child `.swf` or asset requests.
bool ruffle_player_load_swf_bytes(
    RufflePlayerHandle* handle,
    const uint8_t* bytes,
    size_t len,
    const char* base_url_utf8
);

/// Updates the Metal drawable viewport dimensions (in physical pixels) and scale factor.
void ruffle_player_set_viewport_dimensions(
    RufflePlayerHandle* handle,
    uint32_t width,
    uint32_t height,
    double scale_factor
);

/// Polls pending async loader tasks and advances the player by `dt_millis` milliseconds.
void ruffle_player_tick(
    RufflePlayerHandle* handle,
    double dt_millis
);

/// Returns `true` if the stage needs to be re-rendered to the `CAMetalLayer`.
bool ruffle_player_needs_render(const RufflePlayerHandle* handle);

/// Renders the current frame to the `CAMetalLayer` (or offscreen target).
void ruffle_player_render(RufflePlayerHandle* handle);

/// Returns `true` if the player is currently playing.
bool ruffle_player_is_playing(const RufflePlayerHandle* handle);

/// Pauses or resumes playback and audio output.
void ruffle_player_set_is_playing(RufflePlayerHandle* handle, bool playing);

/// Gets the master audio volume (`0.0` to `1.0`).
float ruffle_player_volume(const RufflePlayerHandle* handle);

/// Sets the master audio volume (`0.0` to `1.0`).
void ruffle_player_set_volume(RufflePlayerHandle* handle, float volume);

/// Returns the intrinsic width of the loaded SWF stage in pixels.
uint32_t ruffle_player_movie_width(const RufflePlayerHandle* handle);

/// Returns the intrinsic height of the loaded SWF stage in pixels.
uint32_t ruffle_player_movie_height(const RufflePlayerHandle* handle);

/// Dispatches a touch/mouse press at physical pixel coordinates `(x, y)`.
void ruffle_player_touch_down(RufflePlayerHandle* handle, double x, double y);

/// Dispatches a touch/mouse move at physical pixel coordinates `(x, y)`.
void ruffle_player_touch_move(RufflePlayerHandle* handle, double x, double y);

/// Dispatches a touch/mouse release at physical pixel coordinates `(x, y)`.
void ruffle_player_touch_up(RufflePlayerHandle* handle, double x, double y);

/// Sends UTF-8 text input (from `UIKeyInput.insertText` or hardware keyboard) to the focused text field.
void ruffle_player_text_input(
    RufflePlayerHandle* handle,
    const char* text_utf8
);

/// Sends a Backspace key press and text control event (`UIKeyInput.deleteBackward`).
void ruffle_player_key_backspace(RufflePlayerHandle* handle);

/// Sends an Enter/Return key press and text control event.
void ruffle_player_key_enter(RufflePlayerHandle* handle);

/// Returns `true` if an editable `TextField` (`EditText`) currently has stage focus.
bool ruffle_player_is_text_focused(RufflePlayerHandle* handle);

/// Calls a callback registered by ActionScript via `ExternalInterface.addCallback(name, fn)`.
/// `args_json_utf8` should be a JSON array of arguments (e.g. `"[\"arg1\", 123]"` or `"[]"`).
/// Returns a newly allocated JSON string with the return value; free with `ruffle_string_free`.
char* ruffle_player_call_external_interface(
    RufflePlayerHandle* handle,
    const char* name_utf8,
    const char* args_json_utf8
);

/// Frees a C string returned by `ruffle_player_call_external_interface`.
void ruffle_string_free(char* ptr);

/// Flushes shared objects and destroys the `RufflePlayerHandle`.
void ruffle_player_destroy(RufflePlayerHandle* handle);

#ifdef __cplusplus
}
#endif

#endif /* RUFFLE_APPLE_H */

