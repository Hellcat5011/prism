#![allow(dead_code)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

// Minimal PAM C bindings
#[repr(C)]
struct PamHandle {
    _private: [u8; 0],
}

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: Option<
        unsafe extern "C" fn(
            num_msg: c_int,
            msg: *mut *const PamMessage,
            resp: *mut *mut PamResponse,
            appdata_ptr: *mut c_void,
        ) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_SUCCESS: c_int = 0;

#[link(name = "pam")]
extern "C" {
    fn pam_start(
        service_name: *const c_char,
        user: *const c_char,
        pam_conversation: *const PamConv,
        pamh: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut PamHandle, pam_status: c_int) -> c_int;
}

unsafe extern "C" fn pam_conversation_callback(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() || appdata_ptr.is_null() {
        return 19; // PAM_CONV_ERR
    }

    let password = &*(appdata_ptr as *const String);
    let responses = libc::calloc(num_msg as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if responses.is_null() {
        return 5; // PAM_BUF_ERR
    }

    for i in 0..(num_msg as isize) {
        let msg_ptr = *msg.offset(i);
        let msg_style = (*msg_ptr).msg_style;

        if msg_style == PAM_PROMPT_ECHO_OFF || msg_style == PAM_PROMPT_ECHO_ON {
            let c_password = CString::new(password.as_str()).unwrap_or_default();
            let resp_str = libc::strdup(c_password.as_ptr());
            (*responses.offset(i)).resp = resp_str;
            (*responses.offset(i)).resp_retcode = 0;
        } else {
            (*responses.offset(i)).resp = ptr::null_mut();
            (*responses.offset(i)).resp_retcode = 0;
        }
    }

    *resp = responses;
    PAM_SUCCESS
}

/// Authenticates the given user against PAM.
/// Tries "prism" service first, falls back to "quickshell", "system-local-login", or "login".
pub fn authenticate(user: &str, password: &str) -> bool {
    let services = ["prism", "quickshell", "system-local-login", "login"];
    for service in services {
        if try_authenticate(service, user, password) {
            return true;
        }
    }
    false
}

fn try_authenticate(service: &str, user: &str, password: &str) -> bool {
    let c_service = match CString::new(service) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let c_user = match CString::new(user) {
        Ok(u) => u,
        Err(_) => return false,
    };

    let password_str = password.to_string();
    let conv = PamConv {
        conv: Some(pam_conversation_callback),
        appdata_ptr: &password_str as *const String as *mut c_void,
    };

    unsafe {
        let mut pamh: *mut PamHandle = ptr::null_mut();
        let status = pam_start(c_service.as_ptr(), c_user.as_ptr(), &conv, &mut pamh);
        if status != PAM_SUCCESS || pamh.is_null() {
            return false;
        }

        let auth_status = pam_authenticate(pamh, 0);
        let _ = pam_end(pamh, auth_status);

        auth_status == PAM_SUCCESS
    }
}
