use async_channel::{Receiver, Sender};
use ruffle_core::backend::audio::{AudioBackend, NullAudioBackend};
use ruffle_core::backend::navigator::{
    ErrorResponse, NavigationMethod, NavigatorBackend, OwnedFuture, Request, SocketMode,
    SuccessResponse,
};
use ruffle_core::backend::storage::{MemoryStorageBackend, StorageBackend};
use ruffle_core::backend::ui::{
    DialogResultFuture, FileDialogResult, FileFilter, FontDefinition, FullscreenError,
    LanguageIdentifier, MouseCursor, MultiDialogResultFuture, MultiFileDialogResult, US_ENGLISH,
    UiBackend,
};
use ruffle_core::config::Letterbox;
use ruffle_core::context::UpdateContext;
use ruffle_core::events::{
    KeyDescriptor, KeyLocation, LogicalKey, MouseButton, NamedKey, PhysicalKey, PlayerEvent,
    TextControlCode,
};
use ruffle_core::external::{ExternalInterfaceProvider, Value as ExternalValue};
use ruffle_core::font::FontQuery;
use ruffle_core::indexmap::IndexMap;
use ruffle_core::loader::Error as LoaderError;
use ruffle_core::socket::{SocketAction, SocketHandle};
use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::{
    FloatDuration, LoadBehavior, Player, PlayerBuilder, StageAlign, StageScaleMode,
    ViewportDimensions,
};
use ruffle_frontend_utils::backends::audio::CpalAudioBackend;
use ruffle_frontend_utils::backends::navigator::{
    ExternalNavigatorBackend, FutureSpawner, NavigatorInterface,
};
use ruffle_frontend_utils::backends::storage::DiskStorageBackend;
use ruffle_frontend_utils::content::{ContentDescriptor, PlayingContent};
use ruffle_render::backend::RenderBackend;
use ruffle_render::quality::StageQuality;
use ruffle_render_wgpu::backend::WgpuRenderBackend;
use ruffle_video_software::backend::SoftwareVideoBackend;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::ffi::{CStr, CString, c_char, c_void};
use std::fs::File;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use url::{ParseError, Url};

static TOKIO_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn tokio_runtime() -> &'static tokio::runtime::Runtime {
    TOKIO_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to initialize Tokio runtime for Ruffle Apple SDK")
    })
}

pub type RuffleExternalCallCallback = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        name_utf8: *const c_char,
        args_json_utf8: *const c_char,
    ),
>;

pub type RuffleVirtualKeyboardCallback =
    Option<unsafe extern "C" fn(user_data: *mut c_void, is_open: bool)>;

pub type RuffleNavigateUrlCallback =
    Option<unsafe extern "C" fn(user_data: *mut c_void, url_utf8: *const c_char)>;

#[derive(Clone, Copy)]
struct ExternalCallbackRegistration {
    callback: RuffleExternalCallCallback,
    user_data: *mut c_void,
}

unsafe impl Send for ExternalCallbackRegistration {}
unsafe impl Sync for ExternalCallbackRegistration {}

#[derive(Clone, Copy)]
struct VirtualKeyboardCallbackRegistration {
    callback: RuffleVirtualKeyboardCallback,
    user_data: *mut c_void,
}

unsafe impl Send for VirtualKeyboardCallbackRegistration {}
unsafe impl Sync for VirtualKeyboardCallbackRegistration {}

#[derive(Clone, Copy)]
struct NavigateUrlCallbackRegistration {
    callback: RuffleNavigateUrlCallback,
    user_data: *mut c_void,
}

unsafe impl Send for NavigateUrlCallbackRegistration {}
unsafe impl Sync for NavigateUrlCallbackRegistration {}

struct AppleUiBackend {
    keyboard_reg: Arc<Mutex<VirtualKeyboardCallbackRegistration>>,
    clipboard: String,
    mouse_visible: bool,
}

impl AppleUiBackend {
    fn new(keyboard_reg: Arc<Mutex<VirtualKeyboardCallbackRegistration>>) -> Self {
        Self {
            keyboard_reg,
            clipboard: String::new(),
            mouse_visible: true,
        }
    }
}

impl UiBackend for AppleUiBackend {
    fn mouse_visible(&self) -> bool {
        self.mouse_visible
    }

    fn set_mouse_visible(&mut self, visible: bool) {
        self.mouse_visible = visible;
    }

    fn set_mouse_cursor(&mut self, _cursor: MouseCursor) {}

    fn clipboard_content(&mut self) -> String {
        self.clipboard.clone()
    }

    fn set_clipboard_content(&mut self, content: String) {
        self.clipboard = content;
    }

    fn set_fullscreen(&mut self, _is_full: bool) -> Result<(), FullscreenError> {
        Ok(())
    }

    fn display_root_movie_download_failed_message(&self, _invalid_swf: bool, _fetch_error: String) {
    }

    fn message(&self, _message: &str) {}

    fn display_unsupported_video(&self, _url: Url) {}

    fn load_device_font(&self, _query: &FontQuery, _register: &mut dyn FnMut(FontDefinition)) {}

    fn sort_device_fonts(
        &self,
        _query: &FontQuery,
        _register: &mut dyn FnMut(FontDefinition),
    ) -> Vec<FontQuery> {
        Vec::new()
    }

    fn open_virtual_keyboard(&self) {
        if let Ok(reg) = self.keyboard_reg.lock()
            && let Some(cb) = reg.callback
        {
            unsafe {
                cb(reg.user_data, true);
            }
        }
    }

    fn close_virtual_keyboard(&self) {
        if let Ok(reg) = self.keyboard_reg.lock()
            && let Some(cb) = reg.callback
        {
            unsafe {
                cb(reg.user_data, false);
            }
        }
    }

    fn language(&self) -> LanguageIdentifier {
        US_ENGLISH.clone()
    }

    fn display_file_open_dialog(
        &mut self,
        _filters: Vec<FileFilter>,
    ) -> Option<DialogResultFuture> {
        Some(Box::pin(async move { Ok(FileDialogResult::Canceled) }))
    }

    fn display_file_open_dialog_multiple(
        &mut self,
        _filters: Vec<FileFilter>,
    ) -> Option<MultiDialogResultFuture> {
        Some(Box::pin(async move { Ok(MultiFileDialogResult::Canceled) }))
    }

    fn close_file_dialog(&mut self) {}

    fn display_file_save_dialog(
        &mut self,
        _file_name: String,
        _domain: String,
    ) -> Option<DialogResultFuture> {
        None
    }
}

#[derive(Clone)]
struct AppleFutureSpawner {
    sender: Sender<async_task::Runnable>,
}

impl<E: std::error::Error + 'static> FutureSpawner<E> for AppleFutureSpawner {
    fn spawn(&self, future: OwnedFuture<(), E>) {
        let future = async move {
            if let Err(e) = future.await {
                tracing::error!("Ruffle async loader error: {}", e);
            }
        };
        let sender = self.sender.clone();
        let scheduler = move |runnable| {
            let _ = sender.send_blocking(runnable);
        };
        let (runnable, task) = unsafe { async_task::spawn_unchecked(future, scheduler) };
        task.detach();
        runnable.schedule();
    }
}

#[derive(Clone)]
struct AppleNavigatorInterface {
    navigate_reg: Arc<Mutex<NavigateUrlCallbackRegistration>>,
}

impl NavigatorInterface for AppleNavigatorInterface {
    fn navigate_to_website(&self, url: Url) {
        tracing::info!("SWF requested navigate_to_website: {}", url);
        if let Ok(reg) = self.navigate_reg.lock()
            && let Some(cb) = reg.callback
            && let Ok(c_url) = CString::new(url.as_str())
        {
            unsafe {
                cb(reg.user_data, c_url.as_ptr());
            }
        }
    }

    async fn open_file(&self, path: &Path) -> io::Result<File> {
        File::open(path)
    }

    async fn confirm_socket(&self, _host: &str, _port: u16) -> bool {
        true
    }
}

type InnerNavigator = ExternalNavigatorBackend<AppleFutureSpawner, AppleNavigatorInterface>;

#[derive(Clone)]
struct DynamicNavigatorBackend {
    inner: Rc<RefCell<InnerNavigator>>,
    navigate_reg: Arc<Mutex<NavigateUrlCallbackRegistration>>,
}

impl DynamicNavigatorBackend {
    fn new(
        base_url: Url,
        content: PlayingContent,
        spawner: AppleFutureSpawner,
        navigate_reg: Arc<Mutex<NavigateUrlCallbackRegistration>>,
    ) -> Self {
        let nav = ExternalNavigatorBackend::new(
            base_url,
            None,
            None,
            spawner,
            None,
            true,
            HashSet::new(),
            SocketMode::Allow,
            Rc::new(content),
            AppleNavigatorInterface {
                navigate_reg: navigate_reg.clone(),
            },
        );
        Self {
            inner: Rc::new(RefCell::new(nav)),
            navigate_reg,
        }
    }

    fn reconfigure(&self, base_url: Url, content: PlayingContent, spawner: AppleFutureSpawner) {
        let nav = ExternalNavigatorBackend::new(
            base_url,
            None,
            None,
            spawner,
            None,
            true,
            HashSet::new(),
            SocketMode::Allow,
            Rc::new(content),
            AppleNavigatorInterface {
                navigate_reg: self.navigate_reg.clone(),
            },
        );
        *self.inner.borrow_mut() = nav;
    }
}

impl NavigatorBackend for DynamicNavigatorBackend {
    fn navigate_to_url(
        &self,
        url: &str,
        target: &str,
        vars_method: Option<(NavigationMethod, IndexMap<String, String>)>,
    ) {
        self.inner
            .borrow()
            .navigate_to_url(url, target, vars_method);
    }

    fn fetch(&self, request: Request) -> OwnedFuture<Box<dyn SuccessResponse>, ErrorResponse> {
        self.inner.borrow().fetch(request)
    }

    fn resolve_url(&self, url: &str) -> Result<Url, ParseError> {
        self.inner.borrow().resolve_url(url)
    }

    fn spawn_future(&mut self, future: OwnedFuture<(), LoaderError>) {
        self.inner.borrow_mut().spawn_future(future);
    }

    fn pre_process_url(&self, url: Url) -> Url {
        self.inner.borrow().pre_process_url(url)
    }

    fn connect_socket(
        &mut self,
        host: String,
        port: u16,
        timeout: Duration,
        handle: SocketHandle,
        receiver: Receiver<Vec<u8>>,
        sender: Sender<SocketAction>,
    ) {
        self.inner
            .borrow_mut()
            .connect_socket(host, port, timeout, handle, receiver, sender);
    }
}

struct AppleExternalInterfaceProvider {
    callback_reg: Arc<Mutex<ExternalCallbackRegistration>>,
    spoof_url: Arc<Mutex<Option<Url>>>,
}

impl ExternalInterfaceProvider for AppleExternalInterfaceProvider {
    fn call_method(
        &self,
        _context: &mut UpdateContext<'_>,
        name: &str,
        args: &[ExternalValue],
    ) -> ExternalValue {
        if let Ok(guard) = self.spoof_url.lock()
            && let Some(ref url) = *guard
        {
            if matches!(
                name,
                "document.location.href.toString"
                    | "window.location.href.toString"
                    | "top.location.href.toString"
            ) {
                return ExternalValue::String(url.to_string());
            }
            if name == "eval"
                && let [ExternalValue::String(code)] = args
                && matches!(
                    code.as_str(),
                    "document.location.href" | "window.location.href" | "top.location.href"
                )
            {
                return ExternalValue::String(url.to_string());
            }
        }

        if let Ok(reg) = self.callback_reg.lock()
            && let Some(cb) = reg.callback
        {
            let json_args: Vec<serde_json::Value> =
                args.iter().map(external_value_to_json).collect();
            if let (Ok(c_name), Ok(c_json)) = (
                CString::new(name),
                CString::new(serde_json::to_string(&json_args).unwrap_or_else(|_| "[]".into())),
            ) {
                unsafe {
                    cb(reg.user_data, c_name.as_ptr(), c_json.as_ptr());
                }
            }
        }
        ExternalValue::Null
    }

    fn on_callback_available(&self, _name: &str) {}

    fn get_id(&self) -> Option<String> {
        Some("ruffle_apple".to_string())
    }
}

fn external_value_to_json(val: &ExternalValue) -> serde_json::Value {
    match val {
        ExternalValue::Undefined | ExternalValue::Null => serde_json::Value::Null,
        ExternalValue::Bool(b) => serde_json::Value::Bool(*b),
        ExternalValue::Number(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        ExternalValue::String(s) => serde_json::Value::String(s.clone()),
        ExternalValue::Object(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                obj.insert(k.clone(), external_value_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
        ExternalValue::List(list) => {
            serde_json::Value::Array(list.iter().map(external_value_to_json).collect())
        }
    }
}

fn json_to_external_value(val: &serde_json::Value) -> ExternalValue {
    match val {
        serde_json::Value::Null => ExternalValue::Null,
        serde_json::Value::Bool(b) => ExternalValue::Bool(*b),
        serde_json::Value::Number(n) => ExternalValue::Number(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => ExternalValue::String(s.clone()),
        serde_json::Value::Array(arr) => {
            ExternalValue::List(arr.iter().map(json_to_external_value).collect())
        }
        serde_json::Value::Object(obj) => {
            let mut map = BTreeMap::new();
            for (k, v) in obj {
                map.insert(k.clone(), json_to_external_value(v));
            }
            ExternalValue::Object(map)
        }
    }
}

pub struct RufflePlayerHandle {
    player: Arc<Mutex<Player>>,
    navigator: DynamicNavigatorBackend,
    spawner: AppleFutureSpawner,
    task_receiver: Receiver<async_task::Runnable>,
    callback_reg: Arc<Mutex<ExternalCallbackRegistration>>,
    keyboard_reg: Arc<Mutex<VirtualKeyboardCallbackRegistration>>,
    navigate_reg: Arc<Mutex<NavigateUrlCallbackRegistration>>,
    spoof_url: Arc<Mutex<Option<Url>>>,
    scale_factor: f64,
    single_sample: Option<bool>,
    last_tick_ms: f64,
    last_render_ms: f64,
}

impl RufflePlayerHandle {
    fn drain_tasks(&self) {
        let _guard = tokio_runtime().enter();
        while let Ok(runnable) = self.task_receiver.try_recv() {
            runnable.run();
        }
    }

    fn sync_sample_count(player: &mut Player, scale_factor: f64, last_single: &mut Option<bool>) {
        let sample_count = player.quality().sample_count();
        let single = sample_count > 1 && f64::from(sample_count).sqrt() <= scale_factor;
        if *last_single != Some(single) {
            *last_single = Some(single);
            let max_samples = if single { 1 } else { u32::MAX };
            if let Some(renderer) = <dyn std::any::Any>::downcast_mut::<
                WgpuRenderBackend<ruffle_render_wgpu::target::SwapChainTarget>,
            >(player.renderer_mut())
            {
                renderer.set_max_sample_count(max_samples);
            } else if let Some(renderer) = <dyn std::any::Any>::downcast_mut::<
                WgpuRenderBackend<ruffle_render_wgpu::target::TextureTarget>,
            >(player.renderer_mut())
            {
                renderer.set_max_sample_count(max_samples);
            }
        }
    }
}

/// Creates a new Ruffle Player instance backed by a `CAMetalLayer` (or offscreen Metal texture if `metal_layer_ptr` is null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_create(
    metal_layer_ptr: *mut c_void,
    width: u32,
    height: u32,
    scale_factor: f64,
    storage_dir_utf8: *const c_char,
) -> *mut RufflePlayerHandle {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let safe_width = width.max(1);
        let safe_height = height.max(1);
        let safe_scale = if scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };

        let initial_quality = StageQuality::High;
        let initial_single = f64::from(initial_quality.sample_count()).sqrt() <= safe_scale;

        let renderer: Box<dyn RenderBackend> = if !metal_layer_ptr.is_null() {
            let target = wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(metal_layer_ptr);
            let mut backend = unsafe {
                WgpuRenderBackend::for_window_unsafe(
                    target,
                    (safe_width, safe_height),
                    wgpu::Backends::METAL,
                    wgpu::PowerPreference::HighPerformance,
                    None,
                )
            }
            .expect("Failed to create Metal WgpuRenderBackend for CAMetalLayer");
            if initial_single {
                backend.set_max_sample_count(1);
            }
            Box::new(backend)
        } else {
            let mut backend = WgpuRenderBackend::for_offscreen(
                (safe_width, safe_height),
                wgpu::Backends::METAL,
                wgpu::PowerPreference::HighPerformance,
            )
            .expect("Failed to create offscreen Metal WgpuRenderBackend");
            if initial_single {
                backend.set_max_sample_count(1);
            }
            Box::new(backend)
        };

        let audio: Box<dyn AudioBackend> = match CpalAudioBackend::new(None) {
            Ok(audio) => Box::new(audio),
            Err(e) => {
                tracing::warn!("Falling back to NullAudioBackend: {}", e);
                Box::new(NullAudioBackend::new())
            }
        };

        let storage: Box<dyn StorageBackend> = if !storage_dir_utf8.is_null() {
            let path_str = unsafe { CStr::from_ptr(storage_dir_utf8) }.to_string_lossy();
            Box::new(DiskStorageBackend::new(PathBuf::from(path_str.as_ref())))
        } else {
            Box::new(MemoryStorageBackend::new())
        };

        let (task_sender, task_receiver) = async_channel::unbounded();
        let spawner = AppleFutureSpawner {
            sender: task_sender,
        };

        let callback_reg = Arc::new(Mutex::new(ExternalCallbackRegistration {
            callback: None,
            user_data: std::ptr::null_mut(),
        }));
        let keyboard_reg = Arc::new(Mutex::new(VirtualKeyboardCallbackRegistration {
            callback: None,
            user_data: std::ptr::null_mut(),
        }));
        let navigate_reg = Arc::new(Mutex::new(NavigateUrlCallbackRegistration {
            callback: None,
            user_data: std::ptr::null_mut(),
        }));
        let spoof_url = Arc::new(Mutex::new(None));

        let default_url = Url::parse("file:///").expect("valid root file URL");
        let initial_content =
            PlayingContent::DirectFile(ContentDescriptor::new_remote(default_url.clone()));
        let navigator = DynamicNavigatorBackend::new(
            default_url,
            initial_content,
            spawner.clone(),
            navigate_reg.clone(),
        );

        let external_provider = AppleExternalInterfaceProvider {
            callback_reg: callback_reg.clone(),
            spoof_url: spoof_url.clone(),
        };

        let ui_backend = AppleUiBackend::new(keyboard_reg.clone());

        let player = PlayerBuilder::new()
            .with_boxed_renderer(renderer)
            .with_boxed_audio(audio)
            .with_storage(storage)
            .with_navigator(navigator.clone())
            .with_ui(ui_backend)
            .with_video(SoftwareVideoBackend::new())
            .with_external_interface(Box::new(external_provider))
            .with_autoplay(true)
            .with_letterbox(Letterbox::On)
            .with_quality(StageQuality::High)
            .with_align(StageAlign::default(), false)
            .with_scale_mode(StageScaleMode::default(), false)
            .with_load_behavior(LoadBehavior::Streaming)
            .with_viewport_dimensions(safe_width, safe_height, safe_scale)
            .with_default_font(true)
            .build();

        Box::into_raw(Box::new(RufflePlayerHandle {
            player,
            navigator,
            spawner,
            task_receiver,
            callback_reg,
            keyboard_reg,
            navigate_reg,
            spoof_url,
            scale_factor: safe_scale,
            single_sample: Some(initial_single),
            last_tick_ms: 0.0,
            last_render_ms: 0.0,
        }))
    }));

    result.unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_set_external_call_callback(
    handle: *mut RufflePlayerHandle,
    callback: RuffleExternalCallCallback,
    user_data: *mut c_void,
) {
    if handle.is_null() {
        return;
    }
    let handle = unsafe { &*handle };
    if let Ok(mut reg) = handle.callback_reg.lock() {
        reg.callback = callback;
        reg.user_data = user_data;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_set_virtual_keyboard_callback(
    handle: *mut RufflePlayerHandle,
    callback: RuffleVirtualKeyboardCallback,
    user_data: *mut c_void,
) {
    if handle.is_null() {
        return;
    }
    let handle = unsafe { &*handle };
    if let Ok(mut reg) = handle.keyboard_reg.lock() {
        reg.callback = callback;
        reg.user_data = user_data;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_set_navigate_url_callback(
    handle: *mut RufflePlayerHandle,
    callback: RuffleNavigateUrlCallback,
    user_data: *mut c_void,
) {
    if handle.is_null() {
        return;
    }
    let handle = unsafe { &*handle };
    if let Ok(mut reg) = handle.navigate_reg.lock() {
        reg.callback = callback;
        reg.user_data = user_data;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_load_swf_path(
    handle: *mut RufflePlayerHandle,
    file_path_utf8: *const c_char,
) -> bool {
    if handle.is_null() || file_path_utf8.is_null() {
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let handle = unsafe { &mut *handle };
        let path_str = unsafe { CStr::from_ptr(file_path_utf8) }.to_string_lossy();
        let path = Path::new(path_str.as_ref());
        let abs_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        let Some(content_desc) =
            ContentDescriptor::new_local(&abs_path, abs_path.parent().map(Path::to_path_buf))
        else {
            return false;
        };

        let movie_url = content_desc.url.clone();
        if let Ok(mut spoof) = handle.spoof_url.lock() {
            *spoof = Some(movie_url.clone());
        }

        handle.navigator.reconfigure(
            movie_url.clone(),
            PlayingContent::DirectFile(content_desc),
            handle.spawner.clone(),
        );

        if let Ok(mut player) = handle.player.lock() {
            player.fetch_root_movie(movie_url.to_string(), Vec::new(), Box::new(|_| {}));
        }
        handle.drain_tasks();
        true
    }))
    .unwrap_or(false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_load_swf_bytes(
    handle: *mut RufflePlayerHandle,
    bytes: *const u8,
    len: usize,
    base_url_utf8: *const c_char,
) -> bool {
    if handle.is_null() || bytes.is_null() || len == 0 {
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let handle = unsafe { &mut *handle };
        let slice = unsafe { std::slice::from_raw_parts(bytes, len) };

        let base_url = if !base_url_utf8.is_null() {
            let s = unsafe { CStr::from_ptr(base_url_utf8) }.to_string_lossy();
            Url::parse(&s)
                .or_else(|_| Url::from_file_path(s.as_ref()).map_err(|()| ParseError::EmptyHost))
                .unwrap_or_else(|_| Url::parse("file:///movie.swf").expect("valid fallback URL"))
        } else {
            Url::parse("file:///movie.swf").expect("valid fallback URL")
        };

        let Ok(movie) = SwfMovie::from_data(slice, base_url.to_string(), None, None) else {
            return false;
        };

        if let Ok(mut spoof) = handle.spoof_url.lock() {
            *spoof = Some(base_url.clone());
        }

        let content_desc = ContentDescriptor::new_remote(base_url.clone());
        handle.navigator.reconfigure(
            base_url,
            PlayingContent::DirectFile(content_desc),
            handle.spawner.clone(),
        );

        if let Ok(mut player) = handle.player.lock() {
            player.mutate_with_update_context(|context| {
                context.replace_root_movie(movie);
            });
        }
        handle.drain_tasks();
        true
    }))
    .unwrap_or(false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_set_viewport_dimensions(
    handle: *mut RufflePlayerHandle,
    width: u32,
    height: u32,
    scale_factor: f64,
) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &mut *handle };
        let safe_scale = if scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        handle.scale_factor = safe_scale;
        if let Ok(mut player) = handle.player.lock() {
            RufflePlayerHandle::sync_sample_count(
                &mut player,
                handle.scale_factor,
                &mut handle.single_sample,
            );
            player.set_viewport_dimensions(ViewportDimensions {
                width: width.max(1),
                height: height.max(1),
                scale_factor: safe_scale,
            });
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_tick(handle: *mut RufflePlayerHandle, dt_millis: f64) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let handle = unsafe { &mut *handle };
        let start = Instant::now();
        handle.drain_tasks();
        if let Ok(mut player) = handle.player.lock() {
            player.tick(FloatDuration::from_millis(dt_millis.max(0.0)));
            RufflePlayerHandle::sync_sample_count(
                &mut player,
                handle.scale_factor,
                &mut handle.single_sample,
            );
        }
        handle.drain_tasks();
        handle.last_tick_ms = start.elapsed().as_secs_f64() * 1000.0;
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_needs_render(handle: *const RufflePlayerHandle) -> bool {
    if handle.is_null() {
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &*handle };
        handle
            .player
            .lock()
            .map(|p| p.needs_render())
            .unwrap_or(false)
    }))
    .unwrap_or(false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_render(handle: *mut RufflePlayerHandle) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &mut *handle };
        let start = Instant::now();
        if let Ok(mut player) = handle.player.lock() {
            player.render();
        }
        handle.last_render_ms = start.elapsed().as_secs_f64() * 1000.0;
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_is_playing(handle: *const RufflePlayerHandle) -> bool {
    if handle.is_null() {
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &*handle };
        handle
            .player
            .lock()
            .map(|p| p.is_playing())
            .unwrap_or(false)
    }))
    .unwrap_or(false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_set_is_playing(
    handle: *mut RufflePlayerHandle,
    playing: bool,
) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &mut *handle };
        if let Ok(mut player) = handle.player.lock() {
            player.set_is_playing(playing);
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_volume(handle: *const RufflePlayerHandle) -> f32 {
    if handle.is_null() {
        return 0.0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &*handle };
        handle.player.lock().map(|p| p.volume()).unwrap_or(0.0)
    }))
    .unwrap_or(0.0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_set_volume(handle: *mut RufflePlayerHandle, volume: f32) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &mut *handle };
        if let Ok(mut player) = handle.player.lock() {
            player.set_volume(volume.clamp(0.0, 1.0));
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_movie_width(handle: *const RufflePlayerHandle) -> u32 {
    if handle.is_null() {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &*handle };
        handle
            .player
            .lock()
            .map(|mut p| p.movie_width())
            .unwrap_or(0)
    }))
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_movie_height(handle: *const RufflePlayerHandle) -> u32 {
    if handle.is_null() {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &*handle };
        handle
            .player
            .lock()
            .map(|mut p| p.movie_height())
            .unwrap_or(0)
    }))
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_touch_down(
    handle: *mut RufflePlayerHandle,
    x: f64,
    y: f64,
) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let handle = unsafe { &mut *handle };
        if let Ok(mut player) = handle.player.lock() {
            player.set_mouse_in_stage(true);
            player.handle_event(PlayerEvent::MouseMove { x, y });
            player.handle_event(PlayerEvent::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                index: None,
            });
            player.mutate_with_update_context(|context| {
                *context.needs_render = true;
            });
        }
        handle.drain_tasks();
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_touch_move(
    handle: *mut RufflePlayerHandle,
    x: f64,
    y: f64,
) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let handle = unsafe { &mut *handle };
        if let Ok(mut player) = handle.player.lock() {
            player.set_mouse_in_stage(true);
            player.handle_event(PlayerEvent::MouseMove { x, y });
            player.mutate_with_update_context(|context| {
                *context.needs_render = true;
            });
        }
        handle.drain_tasks();
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_touch_up(handle: *mut RufflePlayerHandle, x: f64, y: f64) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let handle = unsafe { &mut *handle };
        if let Ok(mut player) = handle.player.lock() {
            player.set_mouse_in_stage(true);
            player.handle_event(PlayerEvent::MouseMove { x, y });
            player.handle_event(PlayerEvent::MouseUp {
                x,
                y,
                button: MouseButton::Left,
            });
            player.mutate_with_update_context(|context| {
                *context.needs_render = true;
            });
        }
        handle.drain_tasks();
    }));
}

fn char_to_physical_key(ch: char) -> PhysicalKey {
    match ch.to_ascii_lowercase() {
        'a' => PhysicalKey::KeyA,
        'b' => PhysicalKey::KeyB,
        'c' => PhysicalKey::KeyC,
        'd' => PhysicalKey::KeyD,
        'e' => PhysicalKey::KeyE,
        'f' => PhysicalKey::KeyF,
        'g' => PhysicalKey::KeyG,
        'h' => PhysicalKey::KeyH,
        'i' => PhysicalKey::KeyI,
        'j' => PhysicalKey::KeyJ,
        'k' => PhysicalKey::KeyK,
        'l' => PhysicalKey::KeyL,
        'm' => PhysicalKey::KeyM,
        'n' => PhysicalKey::KeyN,
        'o' => PhysicalKey::KeyO,
        'p' => PhysicalKey::KeyP,
        'q' => PhysicalKey::KeyQ,
        'r' => PhysicalKey::KeyR,
        's' => PhysicalKey::KeyS,
        't' => PhysicalKey::KeyT,
        'u' => PhysicalKey::KeyU,
        'v' => PhysicalKey::KeyV,
        'w' => PhysicalKey::KeyW,
        'x' => PhysicalKey::KeyX,
        'y' => PhysicalKey::KeyY,
        'z' => PhysicalKey::KeyZ,
        '0' | ')' => PhysicalKey::Digit0,
        '1' | '!' => PhysicalKey::Digit1,
        '2' | '@' => PhysicalKey::Digit2,
        '3' | '#' => PhysicalKey::Digit3,
        '4' | '$' => PhysicalKey::Digit4,
        '5' | '%' => PhysicalKey::Digit5,
        '6' | '^' => PhysicalKey::Digit6,
        '7' | '&' => PhysicalKey::Digit7,
        '8' | '*' => PhysicalKey::Digit8,
        '9' | '(' => PhysicalKey::Digit9,
        ' ' => PhysicalKey::Space,
        '-' | '_' => PhysicalKey::Minus,
        '=' | '+' => PhysicalKey::Equal,
        '[' | '{' => PhysicalKey::BracketLeft,
        ']' | '}' => PhysicalKey::BracketRight,
        '\\' | '|' => PhysicalKey::Backslash,
        ';' | ':' => PhysicalKey::Semicolon,
        '\'' | '"' => PhysicalKey::Quote,
        ',' | '<' => PhysicalKey::Comma,
        '.' | '>' => PhysicalKey::Period,
        '/' | '?' => PhysicalKey::Slash,
        '`' | '~' => PhysicalKey::Backquote,
        _ => PhysicalKey::Unknown,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_text_input(
    handle: *mut RufflePlayerHandle,
    text_utf8: *const c_char,
) {
    if handle.is_null() || text_utf8.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let handle = unsafe { &mut *handle };
        let text = unsafe { CStr::from_ptr(text_utf8) }.to_string_lossy();
        if let Ok(mut player) = handle.player.lock() {
            for ch in text.chars() {
                if ch == '\n' || ch == '\r' {
                    let enter_key = KeyDescriptor {
                        physical_key: PhysicalKey::Enter,
                        logical_key: LogicalKey::Named(NamedKey::Enter),
                        key_location: KeyLocation::Standard,
                    };
                    player.handle_event(PlayerEvent::KeyDown { key: enter_key });
                    player.handle_event(PlayerEvent::TextInput { codepoint: '\r' });
                    player.handle_event(PlayerEvent::TextControl {
                        code: TextControlCode::Enter,
                    });
                    player.handle_event(PlayerEvent::KeyUp { key: enter_key });
                } else {
                    let key = KeyDescriptor {
                        physical_key: char_to_physical_key(ch),
                        logical_key: LogicalKey::Character(ch),
                        key_location: KeyLocation::Standard,
                    };
                    player.handle_event(PlayerEvent::KeyDown { key });
                    player.handle_event(PlayerEvent::TextInput { codepoint: ch });
                    player.handle_event(PlayerEvent::KeyUp { key });
                }
            }
            player.mutate_with_update_context(|context| {
                *context.needs_render = true;
            });
        }
        handle.drain_tasks();
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_key_backspace(handle: *mut RufflePlayerHandle) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let handle = unsafe { &mut *handle };
        if let Ok(mut player) = handle.player.lock() {
            let backspace_key = KeyDescriptor {
                physical_key: PhysicalKey::Backspace,
                logical_key: LogicalKey::Named(NamedKey::Backspace),
                key_location: KeyLocation::Standard,
            };
            player.handle_event(PlayerEvent::KeyDown { key: backspace_key });
            player.handle_event(PlayerEvent::TextControl {
                code: TextControlCode::Backspace,
            });
            player.handle_event(PlayerEvent::KeyUp { key: backspace_key });
            player.mutate_with_update_context(|context| {
                *context.needs_render = true;
            });
        }
        handle.drain_tasks();
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_key_enter(handle: *mut RufflePlayerHandle) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let handle = unsafe { &mut *handle };
        if let Ok(mut player) = handle.player.lock() {
            let enter_key = KeyDescriptor {
                physical_key: PhysicalKey::Enter,
                logical_key: LogicalKey::Named(NamedKey::Enter),
                key_location: KeyLocation::Standard,
            };
            player.handle_event(PlayerEvent::KeyDown { key: enter_key });
            player.handle_event(PlayerEvent::TextInput { codepoint: '\r' });
            player.handle_event(PlayerEvent::TextControl {
                code: TextControlCode::Enter,
            });
            player.handle_event(PlayerEvent::KeyUp { key: enter_key });
            player.mutate_with_update_context(|context| {
                *context.needs_render = true;
            });
        }
        handle.drain_tasks();
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_is_text_focused(handle: *mut RufflePlayerHandle) -> bool {
    if handle.is_null() {
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &mut *handle };
        let Ok(mut player) = handle.player.lock() else {
            return false;
        };
        player.mutate_with_update_context(|context| {
            context
                .focus_tracker
                .get_as_edit_text()
                .is_some_and(|tf| tf.is_editable())
        })
    }))
    .unwrap_or(false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_call_external_interface(
    handle: *mut RufflePlayerHandle,
    name_utf8: *const c_char,
    args_json_utf8: *const c_char,
) -> *mut c_char {
    if handle.is_null() || name_utf8.is_null() {
        return std::ptr::null_mut();
    }
    catch_unwind(AssertUnwindSafe(|| {
        let _guard = tokio_runtime().enter();
        let handle = unsafe { &mut *handle };
        let name = unsafe { CStr::from_ptr(name_utf8) }.to_string_lossy();
        let parsed_args: Vec<ExternalValue> = if !args_json_utf8.is_null() {
            let args_str = unsafe { CStr::from_ptr(args_json_utf8) }.to_string_lossy();
            match serde_json::from_str::<serde_json::Value>(&args_str) {
                Ok(serde_json::Value::Array(arr)) => {
                    arr.iter().map(json_to_external_value).collect()
                }
                Ok(other) => vec![json_to_external_value(&other)],
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        let result_val = if let Ok(mut player) = handle.player.lock() {
            player.call_internal_interface(&name, parsed_args)
        } else {
            ExternalValue::Null
        };
        handle.drain_tasks();

        let json_str = serde_json::to_string(&external_value_to_json(&result_val))
            .unwrap_or_else(|_| "null".to_string());
        CString::new(json_str)
            .map(CString::into_raw)
            .unwrap_or(std::ptr::null_mut())
    }))
    .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_string_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        unsafe {
            let _ = CString::from_raw(ptr);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_get_frame_timings(
    handle: *const RufflePlayerHandle,
    out_tick_ms: *mut f64,
    out_render_ms: *mut f64,
    out_frames_run: *mut u64,
    out_swf_fps: *mut f64,
) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &*handle };
        unsafe {
            if !out_tick_ms.is_null() {
                *out_tick_ms = handle.last_tick_ms;
            }
            if !out_render_ms.is_null() {
                *out_render_ms = handle.last_render_ms;
            }
            if let Ok(player) = handle.player.lock() {
                if !out_frames_run.is_null() {
                    *out_frames_run = player.frames_run();
                }
                if !out_swf_fps.is_null() {
                    *out_swf_fps = player.frame_rate();
                }
            }
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_get_quality(handle: *mut RufflePlayerHandle) -> u32 {
    if handle.is_null() {
        return 2;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &mut *handle };
        let Ok(mut player) = handle.player.lock() else {
            return 2;
        };
        match player.quality() {
            StageQuality::Low => 0,
            StageQuality::Medium => 1,
            StageQuality::High
            | StageQuality::Best
            | StageQuality::High8x8
            | StageQuality::High8x8Linear
            | StageQuality::High16x16
            | StageQuality::High16x16Linear => 2,
        }
    }))
    .unwrap_or(2)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_set_quality(handle: *mut RufflePlayerHandle, quality: u32) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &mut *handle };
        let stage_quality = match quality {
            0 => StageQuality::Low,
            1 => StageQuality::Medium,
            _ => StageQuality::High,
        };
        if let Ok(mut player) = handle.player.lock() {
            player.set_quality(stage_quality);
            RufflePlayerHandle::sync_sample_count(
                &mut player,
                handle.scale_factor,
                &mut handle.single_sample,
            );
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_is_interpolating(
    handle: *const RufflePlayerHandle,
) -> bool {
    if handle.is_null() {
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &*handle };
        handle
            .player
            .lock()
            .map(|p| p.frame_interpolation())
            .unwrap_or(false)
    }))
    .unwrap_or(false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_set_interpolating(
    handle: *mut RufflePlayerHandle,
    interpolating: bool,
) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &mut *handle };
        if let Ok(mut player) = handle.player.lock() {
            player.set_frame_interpolation(interpolating);
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_release_memory(handle: *mut RufflePlayerHandle) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handle = unsafe { &mut *handle };
        if let Ok(mut player) = handle.player.lock() {
            player.release_memory();
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ruffle_player_destroy(handle: *mut RufflePlayerHandle) {
    if !handle.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            let _ = Box::from_raw(handle);
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn test_headless_loader_swf() {
        let handle = unsafe { ruffle_player_create(std::ptr::null_mut(), 800, 600, 2.0, std::ptr::null()) };
        assert!(!handle.is_null());

        let swf_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("loader.swf");
        if swf_path.exists() {
            let c_path = CString::new(swf_path.to_str().unwrap()).unwrap();
            let loaded = unsafe { ruffle_player_load_swf_path(handle, c_path.as_ptr()) };
            assert!(loaded);

            for _ in 0..5 {
                unsafe {
                    ruffle_player_tick(handle, 16.67);
                    ruffle_player_render(handle);
                }
            }

            let w = unsafe { ruffle_player_movie_width(handle) };
            let h = unsafe { ruffle_player_movie_height(handle) };
            assert!(w > 0 && h > 0, "Expected non-zero movie dimensions for loader.swf, got {w}x{h}");
        }

        unsafe {
            ruffle_player_destroy(handle);
        }
    }

    #[test]
    fn test_touch_username_and_type() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static KEYBOARD_OPEN: AtomicBool = AtomicBool::new(false);
        static GAME_LOADED: AtomicBool = AtomicBool::new(false);

        unsafe extern "C" fn on_kb(_user_data: *mut c_void, is_open: bool) {
            KEYBOARD_OPEN.store(is_open, Ordering::SeqCst);
        }
        unsafe extern "C" fn on_ext(
            _user_data: *mut c_void,
            name_utf8: *const c_char,
            _args_utf8: *const c_char,
        ) {
            let name = unsafe { CStr::from_ptr(name_utf8) }.to_string_lossy();
            if name == "onLoaded" || name == "loaded" {
                GAME_LOADED.store(true, Ordering::SeqCst);
            }
        }

        let handle = unsafe {
            ruffle_player_create(std::ptr::null_mut(), 2622, 1206, 3.0, std::ptr::null())
        };
        assert!(!handle.is_null());

        unsafe {
            ruffle_player_set_virtual_keyboard_callback(handle, Some(on_kb), std::ptr::null_mut());
            ruffle_player_set_external_call_callback(handle, Some(on_ext), std::ptr::null_mut());
        }

        let swf_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("loader.swf");
        let c_path = CString::new(swf_path.to_str().unwrap()).unwrap();
        assert!(unsafe { ruffle_player_load_swf_path(handle, c_path.as_ptr()) });

        for _ in 0..300 {
            unsafe {
                ruffle_player_tick(handle, 16.67);
            }
            if GAME_LOADED.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        if GAME_LOADED.load(Ordering::SeqCst) {
            // Tick a few more frames so the login screen finishes initializing
            for _ in 0..10 {
                unsafe {
                    ruffle_player_tick(handle, 16.67);
                }
            }

            unsafe {
                ruffle_player_touch_down(handle, 1311.0, 472.0);
                ruffle_player_touch_up(handle, 1311.0, 472.0);
            }

            let focused = unsafe { ruffle_player_is_text_focused(handle) };
            let kb_open = KEYBOARD_OPEN.load(Ordering::SeqCst);
            println!("After tapping Username box: focused={focused}, kb_open={kb_open}");

            let c_text = CString::new("Artix").unwrap();
            unsafe {
                ruffle_player_text_input(handle, c_text.as_ptr());
            }

            let h_ref = unsafe { &*handle };
            let mut player = h_ref.player.lock().unwrap();
            let current_text = player.mutate_with_update_context(|context| {
                context
                    .focus_tracker
                    .get_as_edit_text()
                    .map(|tf| tf.text().to_utf8_lossy().into_owned())
            });
            println!("Focused EditText content after typing: {:?}", current_text);
            assert!(focused, "Expected Username EditText to be focused after tap");
            assert!(kb_open, "Expected virtual keyboard callback to be called with true");
            assert_eq!(current_text.as_deref(), Some("Artix"));
        }

        unsafe {
            ruffle_player_destroy(handle);
        }
    }
}

