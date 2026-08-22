// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! Adapter from CEF's permission prompts to the runtime-neutral policy in
//! [`crate::policy`].
//!
//! Media-access requests use the same policy as Chromium permission prompts.
//!
//! Grants are also written back as content settings.
//! `OnRequestMediaAccessPermission` bypasses Chromium's permission manager, so
//! otherwise `enumerateDevices` still sees "not granted": it hides device
//! labels and reports one placeholder per kind, and nothing is persisted.

use cef::{rc::Rc as _, *};

use crate::policy::{self, PermissionKind, RequestSource};

wrap_permission_handler! {
  pub struct TauriCefPermissionHandler {
    webview_label: String,
  }

  impl PermissionHandler {
    fn on_request_media_access_permission(
      &self,
      browser: Option<&mut Browser>,
      frame: Option<&mut Frame>,
      requesting_origin: Option<&CefString>,
      requested_permissions: u32,
      callback: Option<&mut MediaAccessCallback>,
    ) -> ::std::os::raw::c_int {
      use cef::sys::cef_media_access_permission_types_t as bits;

      let Some(callback) = callback else {
        return 0;
      };
      let callback = callback.clone();
      let origin = requesting_origin.map(|origin| origin.to_string()).unwrap_or_default();
      let is_main_frame = frame.map(|frame| frame.is_main() != 0);
      let kinds = policy::media_kinds(requested_permissions);

      let request_context = browser
        .and_then(|browser| browser.host())
        .and_then(|host| host.request_context());
      let content_types = media_content_types(&kinds);

      // A stored grant answers without asking the policy again.
      if let Some(request_context) = request_context.as_ref()
        && !content_types.is_empty()
        && content_types.len() == kinds.len()
        && content_types.iter().all(|content_type| {
          is_allowed(request_context, &origin, *content_type)
        })
      {
        callback.cont(requested_permissions);
        return 1;
      }

      let grant_recorder = GrantRecorder {
        request_context,
        origin: origin.clone(),
        content_types,
      };

      policy::dispatch(
        &self.webview_label,
        &origin,
        RequestSource::MediaAccess,
        kinds,
        is_main_frame,
        move |granted| {
          if granted {
            grant_recorder.record();
          }
          callback.cont(if granted {
            requested_permissions
          } else {
            bits::CEF_MEDIA_PERMISSION_NONE as u32
          });
        },
      );
      1
    }

    fn on_show_permission_prompt(
      &self,
      _browser: Option<&mut Browser>,
      _prompt_id: u64,
      requesting_origin: Option<&CefString>,
      requested_permissions: u32,
      callback: Option<&mut PermissionPromptCallback>,
    ) -> ::std::os::raw::c_int {
      let Some(callback) = callback else {
        return 0;
      };
      let callback = callback.clone();
      let origin = requesting_origin.map(|origin| origin.to_string()).unwrap_or_default();
      policy::dispatch(
        &self.webview_label,
        &origin,
        RequestSource::Prompt,
        policy::prompt_kinds(requested_permissions),
        // CEF reports no frame for permission prompts — they are browser-scoped.
        None,
        move |granted| {
          let result = if granted {
            cef::sys::cef_permission_request_result_t::CEF_PERMISSION_RESULT_ACCEPT
          } else {
            cef::sys::cef_permission_request_result_t::CEF_PERMISSION_RESULT_DENY
          };
          callback.cont(PermissionRequestResult::from(result));
        },
      );
      1
    }
  }
}

/// `getDisplayMedia` is left out: Chromium asks per use, so nothing persists.
fn media_content_types(kinds: &[PermissionKind]) -> Vec<ContentSettingTypes> {
  kinds
    .iter()
    .filter_map(|kind| match kind {
      PermissionKind::Microphone => Some(ContentSettingTypes::MEDIASTREAM_MIC),
      PermissionKind::Camera => Some(ContentSettingTypes::MEDIASTREAM_CAMERA),
      _ => None,
    })
    .collect()
}

fn is_allowed(
  request_context: &RequestContext,
  origin: &str,
  content_type: ContentSettingTypes,
) -> bool {
  let origin = CefString::from(origin);
  request_context.content_setting(Some(&origin), Some(&origin), content_type)
    == ContentSettingValues::ALLOW
}

/// `SetContentSetting` is UI-thread only; the policy may answer from any thread.
struct GrantRecorder {
  request_context: Option<RequestContext>,
  origin: String,
  content_types: Vec<ContentSettingTypes>,
}

impl GrantRecorder {
  fn record(&self) {
    let Some(request_context) = self.request_context.as_ref() else {
      return;
    };
    if self.content_types.is_empty() || self.origin.is_empty() {
      return;
    }

    if cef::currently_on(cef::sys::cef_thread_id_t::TID_UI.into()) != 0 {
      record_grant(request_context, &self.origin, &self.content_types);
      return;
    }

    let mut task = RecordGrantTask::new(
      request_context.clone(),
      self.origin.clone(),
      self.content_types.clone(),
    );
    cef::post_task(cef::sys::cef_thread_id_t::TID_UI.into(), Some(&mut task));
  }
}

fn record_grant(
  request_context: &RequestContext,
  origin: &str,
  content_types: &[ContentSettingTypes],
) {
  let origin = CefString::from(origin);
  for content_type in content_types {
    request_context.set_content_setting(
      Some(&origin),
      Some(&origin),
      *content_type,
      ContentSettingValues::ALLOW,
    );
  }
}

wrap_task! {
  struct RecordGrantTask {
    request_context: RequestContext,
    origin: String,
    content_types: Vec<ContentSettingTypes>,
  }

  impl Task {
    fn execute(&self) {
      record_grant(&self.request_context, &self.origin, &self.content_types);
    }
  }
}
