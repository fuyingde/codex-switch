//! Native HTTPS reads; background-only, no retries or insecure TLS overrides.
use anyhow::{bail, ensure, Context, Result};
use std::ffi::c_void;
use std::io::Write;
use std::time::{Duration, Instant};
use windows_sys::Win32::Networking::WinHttp::*;

struct Handle(*mut c_void);
impl Handle {
    fn new(value: *mut c_void) -> Result<Self> {
        if value.is_null() {
            bail!("网络请求失败：{}", std::io::Error::last_os_error());
        }
        Ok(Self(value))
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn check(value: i32) -> Result<()> {
    if value == 0 {
        bail!("网络请求失败：{}", std::io::Error::last_os_error());
    }
    Ok(())
}
fn time_left(end: Instant) -> Result<i32> {
    let ms = end.saturating_duration_since(Instant::now()).as_millis();
    ensure!(ms > 0, "请求超时");
    Ok(ms.min(5000) as i32)
}

pub fn get(path: &str, timeout: Duration, limit: usize, sink: &mut dyn Write) -> Result<()> {
    request("www.imtr.cn", 443, path, true, timeout, limit, sink)
}

fn request(
    host: &str,
    port: u16,
    path: &str,
    secure: bool,
    timeout: Duration,
    limit: usize,
    sink: &mut dyn Write,
) -> Result<()> {
    let end = Instant::now() + timeout;
    unsafe {
        let session = Handle::new(WinHttpOpen(
            wide("CodexSwitch/Update").as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        ))?;
        check(WinHttpSetTimeouts(
            session.0,
            1000,
            time_left(end)?,
            time_left(end)?,
            time_left(end)?,
        ))?;
        let retries = 0u32;
        check(WinHttpSetOption(
            session.0,
            WINHTTP_OPTION_CONNECT_RETRIES,
            &retries as *const _ as _,
            4,
        ))?;
        let connection = Handle::new(WinHttpConnect(session.0, wide(host).as_ptr(), port, 0))?;
        let request = Handle::new(WinHttpOpenRequest(
            connection.0,
            wide("GET").as_ptr(),
            wide(path).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            if secure { WINHTTP_FLAG_SECURE } else { 0 },
        ))?;
        // Never follow an unexpected redirect to a different host or an HTTP URL.
        let redirect = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
        check(WinHttpSetOption(
            request.0,
            WINHTTP_OPTION_REDIRECT_POLICY,
            &redirect as *const _ as _,
            4,
        ))?;
        let headers = wide("Cache-Control: no-cache\r\nPragma: no-cache\r\n");
        check(WinHttpSendRequest(
            request.0,
            headers.as_ptr(),
            u32::MAX,
            std::ptr::null(),
            0,
            0,
            0,
        ))?;
        time_left(end)?;
        check(WinHttpReceiveResponse(request.0, std::ptr::null_mut()))?;
        let mut status = 0u32;
        let mut length = 4u32;
        check(WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut status as *mut _ as _,
            &mut length,
            std::ptr::null_mut(),
        ))?;
        ensure!(status == 200, "服务器响应 HTTP {status}");
        let mut total = 0usize;
        loop {
            let ms = time_left(end)?;
            check(WinHttpSetTimeouts(request.0, 1000, ms, ms, ms))?;
            let mut buffer = [0u8; 16384];
            let mut read = 0;
            check(WinHttpReadData(
                request.0,
                buffer.as_mut_ptr() as _,
                buffer.len() as u32,
                &mut read,
            ))?;
            time_left(end)?;
            if read == 0 {
                break;
            }
            total = total.checked_add(read as usize).context("文件过大")?;
            ensure!(total <= limit, "响应超出大小限制");
            sink.write_all(&buffer[..read as usize])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    fn server(status: &str, body: &str, delay: u64) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 2048];
            let _ = stream.read(&mut buffer);
            std::thread::sleep(Duration::from_millis(delay));
            let _ = stream.write_all(reply.as_bytes());
        });
        (port, handle)
    }
    #[test]
    fn local_http_bounds_status_size_timeout_and_reads_only_once() {
        for (status, body, delay, limit, ok) in [
            ("200 OK", "{\"version\":\"1.0.0\"}", 0, 1024, true),
            ("503 Unavailable", "bad", 0, 1024, false),
            ("200 OK", "too large", 0, 3, false),
            ("200 OK", "late", 400, 1024, false),
        ] {
            let (port, handle) = server(status, body, delay);
            let mut bytes = Vec::new();
            let start = Instant::now();
            let result = request(
                "127.0.0.1",
                port,
                "/nested/codex/data.json",
                false,
                Duration::from_millis(250),
                limit,
                &mut bytes,
            );
            assert_eq!(result.is_ok(), ok, "{result:?}");
            assert!(start.elapsed() < Duration::from_secs(2));
            handle.join().unwrap();
        }
    }
}
