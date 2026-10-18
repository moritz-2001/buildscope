//! Loopback-only read-only report API and embedded assets. Indexed event pages keep
//! large traces out of the initial browser payload.
use crate::{
    analysis,
    store::{EventQuery, Store},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Component, Path},
    process::{Command, Stdio},
};
use tiny_http::{Header, Method, Response, Server, StatusCode};

/// Serve a local report until interrupted. `None` opens the example-data tour,
/// including for an empty store. Profiling is launched explicitly from the CLI.
pub fn serve(
    store: Store,
    initial: Option<&str>,
    port: u16,
    open: bool,
    mappings: Vec<(String, String)>,
) -> Result<()> {
    let tour = initial.is_none();
    let initial = match initial {
        Some(id) => store.resolve_id(id)?,
        None => store
            .list()?
            .first()
            .and_then(|run| run["id"].as_str())
            .unwrap_or("")
            .to_owned(),
    };
    let server = Server::http(("127.0.0.1", port)).map_err(|e| anyhow::anyhow!("{e}"))?;
    let address = server
        .server_addr()
        .to_ip()
        .context("HTTP listener has no IP address")?;
    let url = if tour {
        format!("http://{address}/?tour=1")
    } else {
        format!("http://{address}/?run={initial}")
    };
    println!("Buildscope report: {url}");
    if open {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = Command::new(opener)
            .arg(&url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    for request in server.incoming_requests() {
        let host = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Host"))
            .map(|h| h.value.as_str())
            .unwrap_or("");
        let allowed =
            host == address.to_string() || host == format!("localhost:{}", address.port());
        if request.method() != &Method::Get || !allowed {
            let _ = request.respond(
                Response::from_string("read-only loopback endpoint")
                    .with_status_code(StatusCode(403)),
            );
            continue;
        }
        let response = match handle(&store, request.url(), &initial, &mappings) {
            Ok(response) => response,
            Err(error) => json_response(json!({"error":format!("{error:#}")}), 400),
        };
        let _ = request.respond(response);
    }
    Ok(())
}

fn handle(
    store: &Store,
    url: &str,
    initial: &str,
    mappings: &[(String, String)],
) -> Result<Response<Box<dyn Read + Send>>> {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let query = parse_query(query)?;
    match path {
        "/" => Ok(asset(
            include_bytes!("web/index.html"),
            "text/html; charset=utf-8",
        )),
        "/app.js" => Ok(asset(
            include_bytes!("web/app.js"),
            "text/javascript; charset=utf-8",
        )),
        "/tour.js" => Ok(asset(
            include_bytes!("web/tour.js"),
            "text/javascript; charset=utf-8",
        )),
        "/style.css" => Ok(asset(
            include_bytes!("web/style.css"),
            "text/css; charset=utf-8",
        )),
        "/api/runs" => Ok(json_response(
            json!({"runs":store.list()?,"initial":initial,"store":store.root.to_string_lossy()}),
            200,
        )),
        "/api/run" => Ok(json_response(
            store.overview(query.get("id").map(String::as_str).unwrap_or(initial))?,
            200,
        )),
        "/api/unit" => {
            let run = store.resolve_id(required(&query, "run")?)?;
            Ok(json_response(
                store.unit_detail(&run, required(&query, "unit")?)?,
                200,
            ))
        }
        "/api/header" => {
            let run = store.resolve_id(required(&query, "run")?)?;
            Ok(json_response(
                store.header_detail(&run, required(&query, "path")?)?,
                200,
            ))
        }
        "/api/scope" => {
            let run = store.resolve_id(required(&query, "run")?)?;
            Ok(json_response(
                store.scope(
                    &run,
                    required(&query, "unit")?,
                    required(&query, "id")?.parse()?,
                )?,
                200,
            ))
        }
        "/api/include-paths" => {
            let run = store.resolve_id(required(&query, "run")?)?;
            Ok(json_response(
                store.include_paths(&run, required(&query, "path")?)?,
                200,
            ))
        }
        "/api/events" => {
            let run = store.resolve_id(required(&query, "run")?)?;
            let unit = required(&query, "unit")?;
            let value = store.events(
                &run,
                unit,
                EventQuery {
                    text: query.get("q").map(String::as_str).unwrap_or(""),
                    activity: query.get("kind").map(String::as_str).unwrap_or(""),
                    offset: query
                        .get("offset")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                    limit: query
                        .get("limit")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(500),
                    order: query.get("sort").map(String::as_str).unwrap_or("duration"),
                },
            )?;
            Ok(json_response(value, 200))
        }
        "/api/header-history" => {
            let run = store.load(required(&query, "run")?)?;
            Ok(json_response(
                json!(store.header_history(&run, required(&query, "key")?)?),
                200,
            ))
        }
        "/api/compare" => Ok(json_response(
            analysis::compare(
                &store.load(required(&query, "before")?)?,
                &store.load(required(&query, "after")?)?,
            ),
            200,
        )),
        "/api/compare-series" => Ok(json_response(
            analysis::compare_series(
                &store.with_label(required(&query, "before_label")?)?,
                &store.with_label(required(&query, "after_label")?)?,
            )?,
            200,
        )),
        "/api/source" => {
            let run = store.load(required(&query, "run")?)?;
            Ok(json_response(
                store.source(&run, required(&query, "path")?, mappings)?,
                200,
            ))
        }
        "/api/artifact" => {
            let run = store.resolve_id(required(&query, "run")?)?;
            let path = Path::new(required(&query, "path")?);
            ensure!(
                path.components()
                    .all(|part| matches!(part, Component::Normal(_))),
                "invalid artifact path"
            );
            let root = store.run_dir(&run)?.canonicalize()?;
            let actual = root.join(path).canonicalize()?;
            ensure!(
                actual.starts_with(&root) && actual.is_file(),
                "artifact is outside this capture"
            );
            Ok(Response::from_file(File::open(actual)?)
                .with_header(
                    Header::from_bytes("Content-Type", "application/octet-stream").unwrap(),
                )
                .with_header(Header::from_bytes("Content-Disposition", "attachment").unwrap())
                .boxed())
        }
        _ => Ok(json_response(json!({"error":"not found"}), 404)),
    }
}

fn asset(bytes: &'static [u8], content_type: &str) -> Response<Box<dyn Read + Send>> {
    Response::from_data(bytes)
        .with_header(Header::from_bytes("Content-Type",content_type).unwrap())
        .with_header(Header::from_bytes("Cache-Control","no-cache").unwrap())
        .with_header(Header::from_bytes("Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'").unwrap())
        .with_header(Header::from_bytes("X-Content-Type-Options","nosniff").unwrap()).boxed()
}

fn json_response(value: Value, status: u16) -> Response<Box<dyn Read + Send>> {
    Response::from_string(value.to_string())
        .with_status_code(StatusCode(status))
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
        .boxed()
}

fn required<'a>(query: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    query
        .get(key)
        .map(String::as_str)
        .with_context(|| format!("missing query parameter {key}"))
}

fn parse_query(query: &str) -> Result<BTreeMap<String, String>> {
    query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Ok((decode_url(key)?, decode_url(value)?))
        })
        .collect()
}

fn decode_url(input: &str) -> Result<String> {
    let mut output = Vec::new();
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => output.push(b' '),
            b'%' => {
                ensure!(index + 2 < bytes.len(), "invalid URL escape");
                let digits = std::str::from_utf8(&bytes[index + 1..index + 3])?;
                output.push(u8::from_str_radix(digits, 16)?);
                index += 2;
            }
            value => output.push(value),
        }
        index += 1;
    }
    Ok(String::from_utf8(output)?)
}
