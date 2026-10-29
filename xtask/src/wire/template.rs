//! A request's path with what differs between accounts, devices and messages taken out, so the
//! same request from two clients reads the same: `/v2/keys/{aci}/{n}`.

/// `path`'s template: a service id as `{aci}` (`{pni}` with its prefix), a number as `{n}`, a
/// long id (base64, hex) as `{id}`; the query's values as `{v}`, its names kept.
pub(crate) fn template(path: &str) -> String {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let segments: Vec<String> = path.split('/').map(segment).collect();
    let mut template = segments.join("/");
    if !query.is_empty() {
        let mut names: Vec<String> = query
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((name, _)) => format!("{name}={{v}}"),
                None => pair.to_owned(),
            })
            .collect();
        names.sort();
        template.push('?');
        template.push_str(&names.join("&"));
    }
    template
}

fn segment(segment: &str) -> String {
    let decoded = segment.replace("%3A", ":").replace("%3a", ":");
    if let Some(rest) = decoded.strip_prefix("PNI:")
        && uuid(rest)
    {
        return "{pni}".to_owned();
    }
    if uuid(&decoded) {
        return "{aci}".to_owned();
    }
    // `<uuid>.<device>`, as some paths name a device.
    if let Some((id, device)) = decoded.split_once('.')
        && uuid(id)
        && device.bytes().all(|byte| byte.is_ascii_digit())
    {
        return "{aci}.{n}".to_owned();
    }
    if !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit()) {
        return "{n}".to_owned();
    }
    if segment.len() >= 16
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_=%+".contains(&byte))
        && segment.bytes().any(|byte| byte.is_ascii_digit())
    {
        return "{id}".to_owned();
    }
    segment.to_owned()
}

fn uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, byte)| match i {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::template;

    #[test]
    fn ids_and_numbers_become_placeholders() {
        let aci = "0f5671ad-c00a-43c9-b3b8-c7d6f960b4e3";
        assert_eq!(template(&format!("/v2/keys/{aci}/1")), "/v2/keys/{aci}/{n}");
        assert_eq!(
            template(&format!("/v1/messages/PNI:{aci}?story=false&urgent=true")),
            "/v1/messages/{pni}?story={v}&urgent={v}"
        );
        assert_eq!(
            template("/attachments/Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5"),
            "/attachments/{id}"
        );
        assert_eq!(template("/v1/websocket/"), "/v1/websocket/");
        assert_eq!(template("/api/v1/queue/empty"), "/api/v1/queue/empty");
    }
}
