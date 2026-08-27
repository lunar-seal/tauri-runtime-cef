use cef::{BrowserView, ImplView, browser_view_get_for_browser};
use tauri_runtime::dpi::{PhysicalPosition, PhysicalSize, Rect};
use tauri_utils::config::Color;

use crate::webview::AppWebview;

use super::scale_factor;

impl AppWebview {
  fn native_wayland_browser_view(&self) -> Option<BrowserView> {
    browser_view_get_for_browser(Some(&mut self.browser.clone()))
  }

  pub(crate) fn native_wayland_set_background_color(&self, color: Option<Color>) {
    let Some(view) = self.native_wayland_browser_view() else {
      return;
    };
    let (r, g, b, a) = color.unwrap_or_default().into();
    view
      .set_background_color(((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32);
  }

  pub(crate) fn native_wayland_bounds(&self) -> Option<Rect> {
    let view = self.native_wayland_browser_view()?;
    let scale = view.window().as_ref().map(scale_factor).unwrap_or(1.0);
    let bounds = view.bounds();
    Some(Rect {
      position: PhysicalPosition::new(
        (bounds.x as f64 * scale).round() as i32,
        (bounds.y as f64 * scale).round() as i32,
      )
      .into(),
      size: PhysicalSize::new(
        (bounds.width.max(0) as f64 * scale).round() as u32,
        (bounds.height.max(0) as f64 * scale).round() as u32,
      )
      .into(),
    })
  }

  pub(crate) fn native_wayland_take_input_focus(&self) {
    if let Some(view) = self.native_wayland_browser_view() {
      view.request_focus();
    }
  }

  pub(crate) fn native_wayland_set_visible(&self, visible: bool) {
    if let Some(view) = self.native_wayland_browser_view() {
      view.set_visible(i32::from(visible));
    }
  }

  pub(crate) fn native_wayland_set_bounds(&self, x: i32, y: i32, width: i32, height: i32) {
    let Some(view) = self.native_wayland_browser_view() else {
      return;
    };
    let scale = view.window().as_ref().map(scale_factor).unwrap_or(1.0);
    view.set_bounds(Some(&cef::Rect {
      x: (x as f64 / scale).round() as i32,
      y: (y as f64 / scale).round() as i32,
      width: (width.max(1) as f64 / scale).round() as i32,
      height: (height.max(1) as f64 / scale).round() as i32,
    }));
  }
}
