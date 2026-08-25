// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! Native Wayland top-level window backed by CEF Views.
//!
//! CEF cannot embed a native browser child into a foreign Wayland window. The
//! supported native path is the one used by `cefsimple`: CEF owns the
//! `CefWindow`, with a `CefBrowserView` filling it.

use std::sync::{
  Arc, Mutex,
  atomic::{AtomicBool, Ordering},
};

use cef::{rc::Rc, *};
use tauri_runtime::{
  Error, UserEvent,
  dpi::{PhysicalPosition, PhysicalSize, Size as TauriSize},
  webview::InitializationScript,
  window::WindowId,
};
use winit::window::WindowLevel;

use crate::{
  runtime::{Message, RuntimeContext},
  window::{AppWindow, AppWindowAttrs, WindowMessage},
};

type BrowserCreated = Box<dyn FnOnce(Browser, NativeWindow)>;
type Emit = Arc<dyn Fn(Event) + Send + Sync>;

const DRAG_REGION_SCRIPT: &str = r#"
(() => {
  const style = document.createElement("style");
  style.textContent = `
    [data-tauri-drag-region]:not([data-tauri-drag-region="false"]) {
      -webkit-app-region: drag;
    }
    [data-tauri-drag-region]:not([data-tauri-drag-region="deep"]) * {
      -webkit-app-region: no-drag;
    }
    [data-tauri-drag-region="false"],
    [data-tauri-drag-region="deep"] :is(a, button, input, select, textarea, label, summary,
      [contenteditable]:not([contenteditable="false"]), [tabindex]:not([tabindex="-1"]),
      [role="button"], [role="link"], [role="menuitem"], [role="tab"], [role="checkbox"],
      [role="radio"], [role="switch"], [role="option"]):not([data-tauri-drag-region]) {
      -webkit-app-region: no-drag;
    }
  `;
  (document.head || document.documentElement).append(style);
})();
"#;

pub(crate) fn drag_region_initialization_script() -> InitializationScript {
  InitializationScript {
    script: DRAG_REGION_SCRIPT.to_string(),
    for_main_frame_only: false,
  }
}

#[derive(Default)]
struct DraggableRegionsState {
  window: Option<Window>,
  regions: Vec<DraggableRegion>,
}

#[derive(Clone, Default)]
struct DraggableRegions(Arc<Mutex<DraggableRegionsState>>);

impl DraggableRegions {
  fn attach(&self, window: Window) {
    let regions = {
      let mut state = self.0.lock().unwrap();
      state.window = Some(window.clone());
      state.regions.clone()
    };
    window.set_draggable_regions((!regions.is_empty()).then_some(regions.as_slice()));
  }

  fn set(&self, regions: Option<&[DraggableRegion]>) {
    let (window, regions) = {
      let mut state = self.0.lock().unwrap();
      state.regions = regions.unwrap_or_default().to_vec();
      (state.window.clone(), state.regions.clone())
    };
    if let Some(window) = window {
      window.set_draggable_regions((!regions.is_empty()).then_some(regions.as_slice()));
    }
  }
}

#[derive(Clone)]
struct WindowState {
  resizable: bool,
  maximizable: bool,
  minimizable: bool,
  closable: bool,
  min_size: Option<TauriSize>,
  max_size: Option<TauriSize>,
}

#[derive(Debug)]
pub(crate) enum Event {
  CloseRequested,
  Destroyed,
  Focused(bool),
  Resized(PhysicalSize<u32>),
  ScaleFactorChanged {
    scale_factor: f64,
    new_inner_size: PhysicalSize<u32>,
  },
}

#[derive(Clone, Copy)]
struct Geometry {
  scale_factor: f64,
  inner_size: PhysicalSize<u32>,
}

#[derive(Clone)]
pub(crate) struct WindowConfig {
  title: String,
  bounds: Rect,
  show_state: ShowState,
  visible: bool,
  frameless: bool,
  always_on_top: bool,
  initial_scale_factor: f64,
  app_id: String,
  emit: Emit,
  state: Arc<Mutex<WindowState>>,
  draggable_regions: DraggableRegions,
}

impl WindowConfig {
  pub(crate) fn new<T: UserEvent>(
    context: &RuntimeContext<T>,
    window_id: WindowId,
    attrs: &AppWindowAttrs,
    size: PhysicalSize<u32>,
    scale_factor: f64,
  ) -> Self {
    let sender = context.sender.clone();
    let proxy = context.proxy.clone();
    let emit = Arc::new(move |event| {
      if sender
        .send(Message::NativeWaylandWindow(window_id, event))
        .is_ok()
      {
        proxy.wake_up();
      }
    });
    let buttons = attrs.inner.enabled_buttons;
    let show_state = if attrs.inner.fullscreen.is_some() {
      ShowState::FULLSCREEN
    } else if attrs.inner.maximized {
      ShowState::MAXIMIZED
    } else {
      ShowState::NORMAL
    };

    Self {
      title: attrs.inner.title.clone(),
      bounds: Rect {
        x: 0,
        y: 0,
        width: (size.width as f64 / scale_factor).round().max(1.0) as i32,
        height: (size.height as f64 / scale_factor).round().max(1.0) as i32,
      },
      show_state,
      visible: attrs.inner.visible,
      frameless: !attrs.inner.decorations,
      always_on_top: attrs.inner.window_level == WindowLevel::AlwaysOnTop,
      initial_scale_factor: scale_factor,
      app_id: crate::config::config().identifier.clone(),
      emit,
      state: Arc::new(Mutex::new(WindowState {
        resizable: attrs.inner.resizable,
        maximizable: buttons.contains(winit::window::WindowButtons::MAXIMIZE),
        minimizable: buttons.contains(winit::window::WindowButtons::MINIMIZE),
        closable: buttons.contains(winit::window::WindowButtons::CLOSE),
        min_size: attrs.inner.min_surface_size,
        max_size: attrs.inner.max_surface_size,
      })),
      draggable_regions: DraggableRegions::default(),
    }
  }

  pub(crate) fn draggable_regions_changed(
    &self,
  ) -> crate::cef_impl::client::DraggableRegionsChanged {
    let draggable_regions = self.draggable_regions.clone();
    Arc::new(move |regions| draggable_regions.set(regions))
  }

  pub(crate) fn is_frameless(&self) -> bool {
    self.frameless
  }
}

#[derive(Clone)]
pub(crate) struct NativeWindow {
  pub(crate) window: Window,
  pub(crate) browser_view: BrowserView,
  allow_close: Arc<AtomicBool>,
  state: Arc<Mutex<WindowState>>,
}

impl NativeWindow {
  pub(crate) fn force_close(&self) {
    self.allow_close.store(true, Ordering::Release);
    self.window.close();
  }

  pub(crate) fn scale_factor(&self) -> f64 {
    scale_factor(&self.window)
  }

  pub(crate) fn physical_bounds(&self) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    physical_bounds(self.window.bounds_in_screen())
  }

  pub(crate) fn physical_inner_size(&self) -> PhysicalSize<u32> {
    physical_bounds(self.window.client_area_bounds_in_screen()).1
  }
}

pub(crate) fn handle_window_message(
  appwindow: &mut AppWindow,
  message: WindowMessage,
) -> Option<WindowMessage> {
  let native = appwindow
    .children
    .first()
    .and_then(|child| child.native_wayland.clone());
  let Some(native) = native else {
    return Some(message);
  };
  let window = &native.window;
  let scale = native.scale_factor();
  let (outer_position, outer_size) = native.physical_bounds();
  let buttons = appwindow.attrs.inner.enabled_buttons;

  match message {
    WindowMessage::ScaleFactor(tx) => _ = tx.send(Ok(scale)),
    WindowMessage::InnerPosition(tx) | WindowMessage::OuterPosition(tx) => {
      _ = tx.send(Ok(outer_position))
    }
    WindowMessage::InnerSize(tx) => _ = tx.send(Ok(native.physical_inner_size())),
    WindowMessage::OuterSize(tx) => _ = tx.send(Ok(outer_size)),
    WindowMessage::IsFullscreen(tx) => _ = tx.send(Ok(window.is_fullscreen() != 0)),
    WindowMessage::IsMinimized(tx) => _ = tx.send(Ok(window.is_minimized() != 0)),
    WindowMessage::IsMaximized(tx) => _ = tx.send(Ok(window.is_maximized() != 0)),
    WindowMessage::IsFocused(tx) => _ = tx.send(Ok(window.is_active() != 0)),
    WindowMessage::IsDecorated(tx) => _ = tx.send(Ok(appwindow.attrs.inner.decorations)),
    WindowMessage::IsResizable(tx) => _ = tx.send(Ok(appwindow.attrs.inner.resizable)),
    WindowMessage::IsMaximizable(tx) => {
      _ = tx.send(Ok(buttons.contains(winit::window::WindowButtons::MAXIMIZE)))
    }
    WindowMessage::IsMinimizable(tx) => {
      _ = tx.send(Ok(buttons.contains(winit::window::WindowButtons::MINIMIZE)))
    }
    WindowMessage::IsClosable(tx) => {
      _ = tx.send(Ok(buttons.contains(winit::window::WindowButtons::CLOSE)))
    }
    WindowMessage::IsVisible(tx) => _ = tx.send(Ok(window.is_visible() != 0)),
    WindowMessage::IsEnabled(tx) => _ = tx.send(Ok(window.is_enabled() != 0)),
    WindowMessage::IsAlwaysOnTop(tx) => _ = tx.send(Ok(window.is_always_on_top() != 0)),
    WindowMessage::Title(tx) => {
      let title = window.title();
      _ = tx.send(Ok(CefString::from(&title).to_string()))
    }
    WindowMessage::RawWindowHandle(tx) => _ = tx.send(Err(Error::FailedToSendMessage)),
    WindowMessage::Center => window.center_window(Some(&cef_size(outer_size, scale))),
    WindowMessage::RequestUserAttention(_) => {}
    WindowMessage::SetEnabled(enabled) => window.set_enabled(i32::from(enabled)),
    WindowMessage::SetResizable(resizable) => {
      appwindow.attrs.inner.resizable = resizable;
      native.state.lock().unwrap().resizable = resizable;
    }
    WindowMessage::SetMaximizable(enabled) => {
      appwindow
        .attrs
        .inner
        .enabled_buttons
        .set(winit::window::WindowButtons::MAXIMIZE, enabled);
      native.state.lock().unwrap().maximizable = enabled;
    }
    WindowMessage::SetMinimizable(enabled) => {
      appwindow
        .attrs
        .inner
        .enabled_buttons
        .set(winit::window::WindowButtons::MINIMIZE, enabled);
      native.state.lock().unwrap().minimizable = enabled;
    }
    WindowMessage::SetClosable(enabled) => {
      appwindow
        .attrs
        .inner
        .enabled_buttons
        .set(winit::window::WindowButtons::CLOSE, enabled);
      native.state.lock().unwrap().closable = enabled;
    }
    WindowMessage::SetTitle(title) => {
      appwindow.attrs.inner.title = title.clone();
      window.set_title(Some(&CefString::from(title.as_str())));
    }
    WindowMessage::Maximize => window.maximize(),
    WindowMessage::Unmaximize | WindowMessage::Unminimize => window.restore(),
    WindowMessage::Minimize => window.minimize(),
    WindowMessage::Show => window.show(),
    WindowMessage::Hide => window.hide(),
    WindowMessage::SetDecorations(_) => {}
    WindowMessage::SetAlwaysOnBottom(_) => {}
    WindowMessage::SetAlwaysOnTop(on_top) => {
      appwindow.attrs.inner.window_level = if on_top {
        WindowLevel::AlwaysOnTop
      } else {
        WindowLevel::Normal
      };
      window.set_always_on_top(i32::from(on_top));
    }
    WindowMessage::SetVisibleOnAllWorkspaces(_) | WindowMessage::SetContentProtected(_) => {}
    WindowMessage::SetSize(size) => window.set_size(Some(&cef_size_from_tauri(size, scale))),
    WindowMessage::SetMinSize(size) => {
      appwindow.attrs.inner.min_surface_size = size.clone();
      native.state.lock().unwrap().min_size = size;
    }
    WindowMessage::SetMaxSize(size) => {
      appwindow.attrs.inner.max_surface_size = size.clone();
      native.state.lock().unwrap().max_size = size;
    }
    WindowMessage::SetSizeConstraints(constraints) => {
      let min_size =
        crate::window::paired_size_constraint(constraints.min_width, constraints.min_height);
      let max_size =
        crate::window::paired_size_constraint(constraints.max_width, constraints.max_height);
      appwindow.attrs.inner.min_surface_size = min_size.clone();
      appwindow.attrs.inner.max_surface_size = max_size.clone();
      let mut state = native.state.lock().unwrap();
      state.min_size = min_size;
      state.max_size = max_size;
    }
    WindowMessage::SetPosition(_position) => {}
    WindowMessage::SetFullscreen(fullscreen) => window.set_fullscreen(i32::from(fullscreen)),
    WindowMessage::SetFocus => {
      window.activate();
      native.browser_view.request_focus();
    }
    WindowMessage::SetFocusable(focusable) => {
      native.browser_view.set_focusable(i32::from(focusable));
    }
    WindowMessage::SetIcon(_)
    | WindowMessage::SetSkipTaskbar(_)
    | WindowMessage::SetShadow(_)
    | WindowMessage::SetCursorGrab(_)
    | WindowMessage::SetCursorVisible(_)
    | WindowMessage::SetCursorIcon(_)
    | WindowMessage::SetCursorPosition(_)
    | WindowMessage::SetIgnoreCursorEvents(_)
    | WindowMessage::SetBadgeCount(..)
    | WindowMessage::SetBadgeLabel(_)
    | WindowMessage::SetOverlayIcon(_)
    | WindowMessage::SetTitleBarStyle(_)
    | WindowMessage::SetTrafficLightPosition(_)
    | WindowMessage::StartDragging
    | WindowMessage::StartResizeDragging(_)
    | WindowMessage::SetProgressBar(_) => {}
    WindowMessage::SetBackgroundColor(color) => {
      appwindow.attrs.background_color = color;
      if let Some(child) = appwindow.children.first() {
        child.set_background_color(color);
      }
    }
    message => return Some(message),
  }
  None
}

fn cef_size(size: PhysicalSize<u32>, scale: f64) -> Size {
  Size {
    width: (size.width as f64 / scale).round().max(1.0) as i32,
    height: (size.height as f64 / scale).round().max(1.0) as i32,
  }
}

fn cef_size_from_tauri(size: TauriSize, scale: f64) -> Size {
  cef_size(size.to_physical::<u32>(scale), scale)
}

wrap_browser_view_delegate! {
  struct NativeBrowserViewDelegate {
    on_created: Arc<Mutex<Option<BrowserCreated>>>,
    allow_close: Arc<AtomicBool>,
    state: Arc<Mutex<WindowState>>,
    draggable_regions: DraggableRegions,
  }

  impl ViewDelegate {}

  impl BrowserViewDelegate {
    fn browser_runtime_style(&self) -> RuntimeStyle {
      RuntimeStyle::CHROME
    }

    fn on_browser_created(
      &self,
      browser_view: Option<&mut BrowserView>,
      browser: Option<&mut Browser>,
    ) {
      let (Some(browser_view), Some(browser)) = (browser_view, browser) else {
        return;
      };
      // Chrome-style Views creates its internal WebView after applying defaults.
      // Reapply the color now so CEF also uses it behind clipped resize frames.
      browser_view.set_background_color(browser_view.background_color());
      if let Some(on_created) = self.on_created.lock().unwrap().take() {
        let Some(window) = browser_view.window() else {
          log::error!("native Wayland browser view has no CEF window");
          return;
        };
        self.draggable_regions.attach(window.clone());
        on_created(
          browser.clone(),
          NativeWindow {
            window,
            browser_view: browser_view.clone(),
            allow_close: self.allow_close.clone(),
            state: self.state.clone(),
          },
        );
      }
    }

    fn on_popup_browser_view_created(
      &self,
      _browser_view: Option<&mut BrowserView>,
      popup_browser_view: Option<&mut BrowserView>,
      _is_devtools: i32,
    ) -> i32 {
      // Sable has one top-level window. A popup policy can redirect navigation;
      // an allowed native popup is closed instead of creating a second window.
      if let Some(browser) = popup_browser_view.and_then(|view| view.browser())
        && let Some(host) = browser.host()
      {
        host.close_browser(1);
      }
      1
    }
  }
}

wrap_window_delegate! {
  struct NativeWindowDelegate {
    browser_view: BrowserView,
    config: WindowConfig,
    allow_close: Arc<AtomicBool>,
    geometry: Arc<Mutex<Option<Geometry>>>,
  }

  impl ViewDelegate {
    fn preferred_size(&self, _view: Option<&mut View>) -> Size {
      Size {
        width: self.config.bounds.width,
        height: self.config.bounds.height,
      }
    }

    fn minimum_size(&self, view: Option<&mut View>) -> Size {
      self
        .config
        .state
        .lock()
        .unwrap()
        .min_size
        .clone()
        .map(|size| cef_size_from_tauri(size, view_scale_factor(view, self.config.initial_scale_factor)))
        .unwrap_or_default()
    }

    fn maximum_size(&self, view: Option<&mut View>) -> Size {
      self
        .config
        .state
        .lock()
        .unwrap()
        .max_size
        .clone()
        .map(|size| cef_size_from_tauri(size, view_scale_factor(view, self.config.initial_scale_factor)))
        .unwrap_or_default()
    }
  }

  impl PanelDelegate {}

  impl WindowDelegate {
    fn on_window_created(&self, window: Option<&mut Window>) {
      let Some(window) = window else { return };
      window.set_to_fill_layout();
      let mut view = View::from(&self.browser_view);
      window.add_child_view(Some(&mut view));
      window.set_title(Some(&CefString::from(self.config.title.as_str())));
      window.set_always_on_top(i32::from(self.config.always_on_top));
      if self.config.visible {
        window.show();
      }
    }

    fn on_window_destroyed(&self, _window: Option<&mut Window>) {
      (self.config.emit)(Event::Destroyed);
    }

    fn on_window_activation_changed(&self, _window: Option<&mut Window>, active: i32) {
      (self.config.emit)(Event::Focused(active != 0));
    }

    fn on_window_bounds_changed(&self, window: Option<&mut Window>, _bounds: Option<&Rect>) {
      let Some(window) = window else { return };
      let geometry = Geometry {
        scale_factor: scale_factor(window),
        inner_size: physical_bounds(window.client_area_bounds_in_screen()).1,
      };
      let previous = self.geometry.lock().unwrap().replace(geometry);

      if previous.is_some_and(|previous| previous.scale_factor != geometry.scale_factor) {
        (self.config.emit)(Event::ScaleFactorChanged {
          scale_factor: geometry.scale_factor,
          new_inner_size: geometry.inner_size,
        });
      }
      if previous.is_none_or(|previous| previous.inner_size != geometry.inner_size) {
        (self.config.emit)(Event::Resized(geometry.inner_size));
      }
    }

    fn initial_bounds(&self, _window: Option<&mut Window>) -> Rect {
      self.config.bounds.clone()
    }

    fn initial_show_state(&self, _window: Option<&mut Window>) -> ShowState {
      self.config.show_state
    }

    fn is_frameless(&self, _window: Option<&mut Window>) -> i32 {
      i32::from(self.config.frameless)
    }

    fn can_resize(&self, _window: Option<&mut Window>) -> i32 {
      i32::from(self.config.state.lock().unwrap().resizable)
    }

    fn can_maximize(&self, _window: Option<&mut Window>) -> i32 {
      i32::from(self.config.state.lock().unwrap().maximizable)
    }

    fn can_minimize(&self, _window: Option<&mut Window>) -> i32 {
      i32::from(self.config.state.lock().unwrap().minimizable)
    }

    fn can_close(&self, _window: Option<&mut Window>) -> i32 {
      if self.allow_close.load(Ordering::Acquire) {
        1
      } else if !self.config.state.lock().unwrap().closable {
        0
      } else {
        (self.config.emit)(Event::CloseRequested);
        0
      }
    }

    fn window_runtime_style(&self) -> RuntimeStyle {
      RuntimeStyle::CHROME
    }

    fn linux_window_properties(
      &self,
      _window: Option<&mut Window>,
      properties: Option<&mut LinuxWindowProperties>,
    ) -> i32 {
      let Some(properties) = properties else { return 0 };
      properties.wayland_app_id = CefString::from(self.config.app_id.as_str());
      1
    }
  }
}

pub(crate) fn create(
  client: &mut Client,
  url: &CefString,
  settings: &BrowserSettings,
  request_context: Option<&mut RequestContext>,
  config: WindowConfig,
  on_created: BrowserCreated,
) -> Option<()> {
  let allow_close = Arc::new(AtomicBool::new(false));
  let mut browser_delegate = NativeBrowserViewDelegate::new(
    Arc::new(Mutex::new(Some(on_created))),
    allow_close.clone(),
    config.state.clone(),
    config.draggable_regions.clone(),
  );
  let browser_view = browser_view_create(
    Some(client),
    Some(url),
    Some(settings),
    None,
    request_context,
    Some(&mut browser_delegate),
  )?;
  let mut window_delegate = NativeWindowDelegate::new(
    browser_view.clone(),
    config,
    allow_close.clone(),
    Arc::new(Mutex::new(None)),
  );
  window_create_top_level(Some(&mut window_delegate))?;
  Some(())
}

fn physical_bounds(bounds: Rect) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
  let bounds = display_convert_screen_rect_to_pixels(Some(&bounds));
  (
    PhysicalPosition::new(bounds.x, bounds.y),
    PhysicalSize::new(bounds.width.max(0) as u32, bounds.height.max(0) as u32),
  )
}

fn scale_factor(window: &Window) -> f64 {
  window
    .display()
    .map(|display| display.device_scale_factor() as f64)
    .unwrap_or(1.0)
}

fn view_scale_factor(view: Option<&mut View>, fallback: f64) -> f64 {
  view
    .and_then(|view| view.window())
    .as_ref()
    .map(scale_factor)
    .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
  use super::*;
  use tauri_runtime::dpi::LogicalSize;

  #[test]
  fn cef_sizes_stay_in_device_independent_pixels() {
    let physical = cef_size(PhysicalSize::new(300, 180), 1.5);
    assert_eq!((physical.width, physical.height), (200, 120));

    let logical = cef_size_from_tauri(TauriSize::Logical(LogicalSize::new(200.0, 120.0)), 1.5);
    assert_eq!((logical.width, logical.height), (200, 120));
  }
}
