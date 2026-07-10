//! Minimal blocking HTTPS GET via WinHTTP (Windows Schannel). Keeps the crate
//! free of OpenSSL/ring so it still type-checks for Windows from any host.

use std::ptr;

use windows_sys::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPTION_CONNECT_TIMEOUT,
    WINHTTP_OPTION_RECEIVE_TIMEOUT, WINHTTP_OPTION_RESOLVE_TIMEOUT, WINHTTP_OPTION_SEND_TIMEOUT,
};

use crate::diag;

/// Fetch `url` (must be `https://…`) and return the response body as UTF-8 lossy text.
pub fn https_get(url: &str, timeout_secs: u32) -> anyhow::Result<String> {
    let (host, path, https) = split_url(url)?;
    if !https {
        anyhow::bail!("only https URLs are supported (got {url})");
    }

    let agent: Vec<u16> = "MusicRpc/3.0\0".encode_utf16().collect();
    let host_w: Vec<u16> = host.encode_utf16().chain(std::iter::once(0)).collect();
    let path_w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            ptr::null(),
            ptr::null(),
            0,
        );
        if session.is_null() {
            anyhow::bail!("WinHttpOpen failed: {}", std::io::Error::last_os_error());
        }

        let timeout_ms = timeout_secs.saturating_mul(1000);
        for opt in [
            WINHTTP_OPTION_RESOLVE_TIMEOUT,
            WINHTTP_OPTION_CONNECT_TIMEOUT,
            WINHTTP_OPTION_SEND_TIMEOUT,
            WINHTTP_OPTION_RECEIVE_TIMEOUT,
        ] {
            let mut ms = timeout_ms;
            let _ = WinHttpSetOption(
                session,
                opt,
                &mut ms as *mut _ as *const _,
                std::mem::size_of_val(&ms) as u32,
            );
        }

        let connect = WinHttpConnect(session, host_w.as_ptr(), 443, 0);
        if connect.is_null() {
            WinHttpCloseHandle(session);
            anyhow::bail!("WinHttpConnect failed: {}", std::io::Error::last_os_error());
        }

        let verb: Vec<u16> = "GET\0".encode_utf16().collect();
        let request = WinHttpOpenRequest(
            connect,
            verb.as_ptr(),
            path_w.as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            WinHttpCloseHandle(connect);
            WinHttpCloseHandle(session);
            anyhow::bail!("WinHttpOpenRequest failed: {}", std::io::Error::last_os_error());
        }

        let headers: Vec<u16> =
            "Referer: https://music.163.com/\r\nUser-Agent: Mozilla/5.0\r\n\0"
                .encode_utf16()
                .collect();

        let sent = WinHttpSendRequest(
            request,
            headers.as_ptr(),
            (headers.len() - 1) as u32,
            ptr::null(),
            0,
            0,
            0,
        );
        if sent == 0 {
            let err = std::io::Error::last_os_error();
            WinHttpCloseHandle(request);
            WinHttpCloseHandle(connect);
            WinHttpCloseHandle(session);
            anyhow::bail!("WinHttpSendRequest failed: {err}");
        }

        if WinHttpReceiveResponse(request, ptr::null_mut()) == 0 {
            let err = std::io::Error::last_os_error();
            WinHttpCloseHandle(request);
            WinHttpCloseHandle(connect);
            WinHttpCloseHandle(session);
            anyhow::bail!("WinHttpReceiveResponse failed: {err}");
        }

        let mut body: Vec<u8> = Vec::new();
        loop {
            let mut available: u32 = 0;
            if WinHttpQueryDataAvailable(request, &mut available) == 0 || available == 0 {
                break;
            }
            let mut chunk = vec![0u8; available as usize];
            let mut read: u32 = 0;
            if WinHttpReadData(request, chunk.as_mut_ptr() as *mut _, available, &mut read) == 0 {
                break;
            }
            chunk.truncate(read as usize);
            body.extend_from_slice(&chunk);
        }

        WinHttpCloseHandle(request);
        WinHttpCloseHandle(connect);
        WinHttpCloseHandle(session);

        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

fn split_url(url: &str) -> anyhow::Result<(String, String, bool)> {
    let (host_path, secure) = if let Some(rest) = url.strip_prefix("https://") {
        (rest, true)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (rest, false)
    } else {
        anyhow::bail!("unsupported URL scheme: {url}");
    };
    let (host, path) = match host_path.split_once('/') {
        Some((h, p)) => (h.to_string(), format!("/{p}")),
        None => (host_path.to_string(), "/".to_string()),
    };
    if host.is_empty() {
        anyhow::bail!("empty host in {url}");
    }
    Ok((host, path, secure))
}

/// Best-effort GET; logs and returns `None` on failure (NetEase metadata is optional).
pub fn https_get_ok(url: &str) -> Option<String> {
    match https_get(url, 5) {
        Ok(body) => Some(body),
        Err(e) => {
            diag!("[http] GET {url} failed: {e}");
            None
        }
    }
}
