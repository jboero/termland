use anyhow::Result;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;

#[repr(C)]
struct PamHandle {
    _opaque: [u8; 0],
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

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        pamh: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut PamHandle, status: c_int) -> c_int;
}

struct ConvData {
    password: CString,
}

unsafe extern "C" fn conversation(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    let data = unsafe { &*(appdata_ptr as *const ConvData) };

    let responses = unsafe {
        libc::calloc(num_msg as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse
    };
    if responses.is_null() {
        return 1; // PAM_BUF_ERR
    }

    for i in 0..num_msg as isize {
        let m = unsafe { &**msg.offset(i) };
        match m.msg_style {
            PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => {
                let pw = unsafe { libc::strdup(data.password.as_ptr()) };
                unsafe { (*responses.offset(i)).resp = pw };
            }
            _ => {}
        }
    }

    unsafe { *resp = responses };
    PAM_SUCCESS
}

/// Authenticate a user via PAM.
///
/// Uses the "termland" PAM service (falls back to "login" if not configured).
pub fn pam_authenticate_user(username: &str, password: &str) -> Result<bool> {
    let service = if std::path::Path::new("/etc/pam.d/termland").exists() {
        "termland"
    } else {
        "login"
    };

    let service_c = CString::new(service)?;
    let user_c = CString::new(username)?;
    let pass_c = CString::new(password)?;

    let mut conv_data = ConvData { password: pass_c };
    let conv = PamConv {
        conv: Some(conversation),
        appdata_ptr: &mut conv_data as *mut ConvData as *mut c_void,
    };

    let mut pamh: *mut PamHandle = ptr::null_mut();

    let rc = unsafe { pam_start(service_c.as_ptr(), user_c.as_ptr(), &conv, &mut pamh) };
    if rc != PAM_SUCCESS {
        return Err(anyhow::anyhow!("pam_start failed: {rc}"));
    }

    let rc = unsafe { pam_authenticate(pamh, 0) };
    let success = rc == PAM_SUCCESS;

    unsafe { pam_end(pamh, rc) };

    if success {
        tracing::info!("PAM: user '{username}' authenticated via '{service}'");
    } else {
        tracing::warn!("PAM: user '{username}' auth failed (rc={rc})");
    }

    Ok(success)
}
