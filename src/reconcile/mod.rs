//! Decides which CUPS changes bring the machine in line with the server. Pure: no I/O.

use crate::api::model::{Connection, PrinterDef, QueueKind};
use crate::cups::{self, Installed, Queue};
use crate::style::{self, BAD, GOOD, KEY, WARN};
use std::collections::{HashMap, HashSet};
use std::fmt::{self, Display, Formatter};
use url::Url;

/// Changes needed to match the server.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Queues to create, or update in place because their URI changed.
    pub add: Vec<Queue>,
    /// Managed queues the server no longer offers. Queues it still offers but papermend skipped
    /// are left alone, since they cannot be recreated.
    pub remove: Vec<String>,
    /// Managed queues already correct.
    pub keep: Vec<Queue>,
    /// Server queues papermend will not install, with the reason.
    pub skipped: Vec<(String, String)>,
}

impl Plan {
    /// Whether applying this plan would change anything.
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty()
    }
}

impl Display for Plan {
    /// One coloured line per queue: skipped, unchanged, added, then removed.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (name, reason) in &self.skipped {
            writeln!(f, "{}", style::line(WARN, "skip", format_args!("{name} ({reason})")))?;
        }
        for queue in &self.keep {
            writeln!(f, "{}", style::line(KEY, "ok", queue))?;
        }
        for queue in &self.add {
            writeln!(f, "{}", style::line(GOOD, "add", queue))?;
        }
        for name in &self.remove {
            writeln!(f, "{}", style::line(BAD, "del", name))?;
        }
        if self.is_empty() {
            writeln!(f, "{}", style::line(KEY, "done", "nothing to change"))?;
        }
        Ok(())
    }
}

/// Compares the server's queues with the managed queues currently in CUPS, as from
/// [`cups::managed`]. `addresses` maps
/// Mobility Print queue names to the user's per-user printer addresses (see `api::mobility`).
pub fn plan(
    desired: &[PrinterDef],
    installed: &[Installed],
    addresses: &HashMap<String, String>,
) -> Plan {
    let mut plan = Plan::default();
    let current: HashMap<&str, &str> =
        installed.iter().map(|q| (q.name.as_str(), q.uri.as_str())).collect();

    let mut wanted = Vec::new();
    for printer in desired {
        match to_queue(printer, addresses) {
            Ok(queue) => wanted.push(queue),
            Err(reason) => plan.skipped.push((printer.name.clone(), reason)),
        }
    }

    for queue in &wanted {
        match current.get(queue.name.as_str()) {
            Some(uri) if *uri == queue.uri => plan.keep.push(queue.clone()),
            _ => plan.add.push(queue.clone()),
        }
    }

    let offered: HashSet<String> = desired.iter().map(|p| cups::queue_name(&p.name)).collect();
    plan.remove = current
        .keys()
        .filter(|name| !offered.contains(**name))
        .map(|name| name.to_string())
        .collect();
    plan.remove.sort();
    plan
}

/// Turns a server queue into a CUPS queue, or explains why it is refused.
fn to_queue(printer: &PrinterDef, addresses: &HashMap<String, String>) -> Result<Queue, String> {
    if printer.optional {
        return Err("optional self-service queue".into());
    }
    // Mobility Print queues use the user's own address and need PostScript.
    let (uri, mobility) = match printer.kind() {
        QueueKind::Server => (device_uri(&printer.connection)?, false),
        QueueKind::Mobility => {
            mobility_server(printer).ok_or("Mobility Print queue has no usable server URL")?;
            let address =
                addresses.get(&printer.name).ok_or("no per-user Mobility Print address")?;
            (address.clone(), true)
        }
        QueueKind::Direct => {
            return Err("direct-print queue: needs PaperCut job tracking for accounting".into());
        }
        QueueKind::Unknown => {
            return Err(format!("unrecognised queue type {:?}", printer.connection.kind));
        }
    };
    // Hyphens read as word breaks in server names, e.g. `Canon-Colour` -> `Canon Colour`.
    let description =
        if printer.description.is_empty() { &printer.name } else { &printer.description };
    let description = description.replace('-', " ");
    Ok(Queue { name: cups::queue_name(&printer.name), uri, description, postscript: mobility })
}

/// The Mobility Print server a queue lives on, e.g. `https://host:9164`, if it is a Mobility
/// Print queue with a usable server URL.
pub fn mobility_server(printer: &PrinterDef) -> Option<Url> {
    if printer.kind() != QueueKind::Mobility {
        return None;
    }
    let server = Url::parse(printer.connection.name.as_deref()?).ok()?;
    matches!(server.scheme(), "https" | "ipps").then_some(server)
}

/// Builds a CUPS device URI from a server connection.
fn device_uri(connection: &Connection) -> Result<String, String> {
    let protocol = connection.protocol.to_ascii_lowercase();
    if protocol == "ipp"
        && let Some(name) = connection.name.as_deref().filter(|n| n.starts_with("ipp"))
    {
        return Ok(name.to_owned());
    }
    let (scheme, segments): (&str, &[&str]) = match protocol.as_str() {
        "ipp" => ("ipp", &["printers"]),
        "samba" | "smb" => ("smb", &[]),
        "lpr" | "lpd" => ("lpd", &[]),
        "pcraw" | "local" => return Err(format!("{protocol:?} connection requires PaperCut")),
        other => return Err(format!("unsupported protocol {other:?}")),
    };
    if connection.host.is_empty() || connection.queue.is_empty() {
        return Err("connection is missing a host or queue".into());
    }

    let mut url = Url::parse(&format!("{scheme}://{}", connection.host))
        .map_err(|e| format!("invalid host {:?}: {e}", connection.host))?;
    url.set_port(connection.port).map_err(|()| "cannot set port".to_owned())?;
    url.path_segments_mut()
        .map_err(|()| "cannot build path".to_owned())?
        .extend(segments)
        .push(&connection.queue);
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn printer(name: &str, kind: &str, protocol: &str) -> PrinterDef {
        PrinterDef {
            name: name.into(),
            connection: Connection {
                kind: kind.into(),
                protocol: protocol.into(),
                host: "ps.example".into(),
                queue: name.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// A managed queue for server queue `name`.
    fn installed(name: &str, uri: &str) -> Installed {
        Installed { name: cups::queue_name(name), uri: uri.into() }
    }

    #[rstest]
    #[case("samba", None, None, "smb://ps.example/Find%20Me")]
    #[case("lpr", None, None, "lpd://ps.example/Find%20Me")]
    #[case("ipp", Some(631), None, "ipp://ps.example:631/printers/Find%20Me")]
    #[case("ipp", None, Some("ipps://ps.example/ipp/x"), "ipps://ps.example/ipp/x")]
    fn uris(
        #[case] protocol: &str,
        #[case] port: Option<u16>,
        #[case] name: Option<&str>,
        #[case] expected: &str,
    ) {
        let connection = Connection {
            protocol: protocol.into(),
            host: "ps.example".into(),
            port,
            queue: "Find Me".into(),
            name: name.map(str::to_owned),
            kind: String::new(),
        };
        assert_eq!(device_uri(&connection).unwrap(), expected);
    }

    #[rstest]
    #[case("pcraw")]
    #[case("local")]
    #[case("raw")]
    #[case("")]
    fn refused_protocols(#[case] protocol: &str) {
        let connection = Connection { protocol: protocol.into(), ..Default::default() };
        assert!(device_uri(&connection).is_err());
    }

    #[test]
    fn adds_new_keeps_same_updates_changed_removes_stale() {
        let desired = [
            printer("New", "SERVER", "samba"),
            printer("Same", "SERVER", "samba"),
            printer("Moved", "SERVER", "samba"),
        ];
        let current = [
            installed("Same", "smb://ps.example/Same"),
            installed("Moved", "smb://old.example/Moved"),
            installed("Stale", "smb://ps.example/Stale"),
        ];
        let plan = plan(&desired, &current, &HashMap::new());
        let names = |queues: &[Queue]| queues.iter().map(|q| q.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&plan.add), [cups::queue_name("New"), cups::queue_name("Moved")]);
        assert_eq!(names(&plan.keep), [cups::queue_name("Same")]);
        assert_eq!(plan.remove, [cups::queue_name("Stale")]);
        assert!(plan.skipped.is_empty());
    }

    /// A Mobility Print queue with its per-user address.
    fn mobility(name: &str) -> (PrinterDef, HashMap<String, String>) {
        let mut printer = printer(name, "", "");
        printer.connection.kind = "mobilityprint".into();
        printer.connection.name = Some("https://m.example:9164".into());
        let address = format!("ipps://m.example:9164/printers/{name}/users/u/0a1b");
        (printer, HashMap::from([(name.to_owned(), address)]))
    }

    #[test]
    fn mobility_queue_uses_stored_address_and_postscript() {
        let (printer, addresses) = mobility("Canon-Colour");
        let plan = plan(&[printer], &[], &addresses);
        assert_eq!(
            plan.add,
            [Queue {
                name: cups::queue_name("Canon-Colour"),
                uri: "ipps://m.example:9164/printers/Canon-Colour/users/u/0a1b".into(),
                description: "Canon Colour".into(),
                postscript: true,
            }]
        );
    }

    #[test]
    fn mobility_queue_without_address_is_skipped() {
        let (printer, _) = mobility("M");
        let plan = plan(&[printer], &[], &HashMap::new());
        assert!(plan.skipped[0].1.contains("per-user"), "{:?}", plan.skipped);
    }

    #[test]
    fn display_name_reads_hyphens_as_spaces() {
        let plan = plan(&[printer("Canon-Colour", "SERVER", "samba")], &[], &HashMap::new());
        assert_eq!(plan.add[0].description, "Canon Colour");
        assert_eq!(plan.add[0].name, cups::queue_name("Canon-Colour"));
    }

    #[test]
    fn mobility_queue_without_server_is_skipped() {
        let mut mobility = printer("M", "", "");
        mobility.connection.kind = "mobilityprint".into();
        assert!(plan(&[mobility], &[], &HashMap::new()).skipped[0].1.contains("server URL"));
    }

    #[rstest]
    #[case("DIRECT", "samba", "direct-print")]
    #[case("weird", "samba", "unrecognised")]
    #[case("SERVER", "pcraw", "requires PaperCut")]
    fn refuses_untrackable_queues(
        #[case] kind: &str,
        #[case] protocol: &str,
        #[case] reason: &str,
    ) {
        let plan = plan(&[printer("Q", kind, protocol)], &[], &HashMap::new());
        assert!(plan.add.is_empty());
        assert!(plan.skipped[0].1.contains(reason), "{:?}", plan.skipped);
    }

    #[test]
    fn optional_queues_are_skipped() {
        let mut optional = printer("Opt", "SERVER", "samba");
        optional.optional = true;
        assert_eq!(plan(&[optional], &[], &HashMap::new()).skipped.len(), 1);
    }

    #[test]
    fn skipped_queue_that_is_installed_is_left_alone() {
        let (printer, _) = mobility("M");
        let current = [installed("M", "ipps://m.example:9164/printers/M/users/u/0a1b")];
        let plan = plan(&[printer], &current, &HashMap::new());
        assert!(plan.remove.is_empty() && plan.add.is_empty(), "{plan:?}");
    }
}
