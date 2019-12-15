use std::{
    net::SocketAddr,
    time::Duration,
};

use clap::{App, AppSettings, Arg, SubCommand};
use serde_json::{Value, json, to_string as stringify};

use tplinker::{
    error::Result as TpResult,
    capabilities::{
        Switch,
        DeviceActions,
    },
    datatypes::{
        DeviceData, SysInfo
    },
    devices::{
        Device,
        RawDevice,
        HS100,
        HS110,
        LB110,
    }
};

fn command_discover(timeout: Option<Duration>, format: Format) -> Vec<Value> {
    tplinker::discovery::with_timeout(timeout).unwrap().into_iter().map(|(addr, data)| {
        let device = Device::from_data(addr, &data);
        format.discover(addr, device, data)
    }).collect()
}

fn command_status(addresses: Vec<SocketAddr>, format: Format) -> Vec<Value> {
    use rayon::prelude::*;
    addresses.into_par_iter().filter_map(|addr|
        device_from_addr(addr)
            .map(|(addr, dev, info)| format.status(addr, dev, info))
            .map_err(|err| eprintln!("While querying {}: {}", addr, err))
            .ok()
    ).collect()
}

fn command_reboot(addresses: Vec<SocketAddr>, delay: Duration, format: Format) -> Vec<Value> {
    use rayon::prelude::*;
    addresses.into_par_iter().filter_map(|addr|
        device_from_addr(addr)
            .map(|(addr, dev, info)| {
                let result = dev.reboot_with_delay(delay)
                    .map(|_| Value::Bool(true))
                    .unwrap_or_else(|err| Value::String(format!("Error: {}", err)));
                format.actioned(addr, dev, info, "Rebooted?", result)
            })
            .map_err(|err| eprintln!("While querying {}: {}", addr, err))
            .ok()
    ).collect()
}

fn device_from_addr(addr: SocketAddr) -> TpResult<(SocketAddr, Device, SysInfo)> {
    let raw = RawDevice::from_addr(addr);
    let info = raw.sysinfo()?;

    // Re-interpret as correct model
    let (dev, info) = if info.model.starts_with("HS100") {
        let dev = unsafe { HS100::from_raw(raw) };
        let info = dev.sysinfo()?;
        (Device::HS100(dev), info)
    } else if info.model.starts_with("HS110") {
        let dev = unsafe { HS110::from_raw(raw) };
        let info = dev.sysinfo()?;
        (Device::HS110(dev), info)
    } else if info.model.starts_with("LB110") {
        let dev = unsafe { LB110::from_raw(raw) };
        let info = dev.sysinfo()?;
        (Device::LB110(dev), info)
    } else {
        (Device::Unknown(raw), info)
    };

    Ok((addr, dev, info))
}

fn pad(value: &str, padding: usize) -> String {
    let pad = " ".repeat(padding.saturating_sub(value.len()));
    format!("{}{}", value, pad)
}

fn device_is_on(device: Device) -> Option<bool> {
    match device {
        Device::HS100(device) => device.is_on().ok(),
        Device::HS110(device) => device.is_on().ok(),
        Device::LB110(device) => device.is_on().ok(),
        _ => None,
    }
}

fn human_stringify(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.to_string(),
        Value::Array(v) => v.iter().map(human_stringify).collect::<Vec<String>>().join(", "),
        Value::Object(o) => stringify(o).unwrap(),
    }
}

#[derive(Clone, Copy, Debug)]
enum Format {
    Short,
    Long,
    JSON,
}

impl Format {
    fn output(self, rows: Vec<Value>) {
        use std::collections::HashMap;

        println!("{}", match self {
            Format::JSON => stringify(&rows).unwrap(),
            Format::Short | Format::Long => {
                // field title -> (ordering, field width)
                let mut fields: HashMap<String, (usize, usize)> = HashMap::new();
                let mut processed: Vec<HashMap<String, String>> = Vec::with_capacity(rows.len());

                for row in rows {
                    let arr = row.as_array().expect("(bug) row is not an array");
                    let mut proc = HashMap::with_capacity(arr.len());

                    for field in arr {
                        if let [key, value] = &field.as_array().expect("(bug) row is not an array of arrays")[..] {
                            let order_next = fields.len();
                            let k = key.as_str().unwrap().to_string();
                            let h = human_stringify(&value);
                            let hlen = h.len();
                            proc.insert(k.clone(), h);
                            fields.entry(k).and_modify(|(_, len)| {
                                if *len < hlen {
                                    *len = hlen;
                                }
                            }).or_insert((order_next, hlen));
                        }
                    }

                    processed.push(proc);
                }

                let mut fields: Vec<(String, (usize, usize))> = fields.into_iter().collect();
                fields.sort_unstable_by(|(_, (a, _)), (_, (b, _))| a.cmp(b));
                let fields: Vec<(String, usize)> = fields.into_iter().map(|(name, (_, width))| (name, width)).collect();
                let mut lines = Vec::with_capacity(processed.len() + 2);

                lines.push(format!(" {} ", fields.iter().map(
                        |(name, width)| pad(name, *width)
                ).collect::<Vec<String>>().join(" | ")));

                lines.push(format!("-{}-", fields.iter().map(
                        |(_, width)| "-".repeat(*width)
                ).collect::<Vec<String>>().join("-+-")));

                lines.extend(processed.into_iter().map(|row|
                    format!(" {} ", fields.iter().map(|(name, width)|
                        pad(row.get(name).unwrap(), *width)
                    ).collect::<Vec<String>>().join(" | "))
                ));

                lines.join("\n")
            }
        })
    }

    fn discover(self, addr: SocketAddr, device: Device, data: DeviceData) -> Value {
        match self {
            Format::JSON => json!({
                "addr": addr,
                "device": Self::device(device),
                "data": data,
            }),
            rest => rest.status(addr, device, data.sysinfo())
        }
    }

    fn status(self, addr: SocketAddr, device: Device, sysinfo: SysInfo) -> Value {
        match self {
            Format::Short => {
                json!([
                    ["Address", addr],
                    ["Alias", sysinfo.alias],
                    ["Product", sysinfo.dev_name],
                    ["Model", sysinfo.model],
                    ["Signal", format!("{} dB", sysinfo.rssi)],
                    ["On?", device_is_on(device)],
                ])
            },
            Format::Long => {
                let (lat, lon) = device.location()
                    .map(|(lat, lon)| (Some(lat), Some(lon)))
                    .unwrap_or((None, None));

                json!([
                    ["Address", addr],
                    ["MAC", sysinfo.mac],
                    ["Alias", sysinfo.alias],
                    ["Product", sysinfo.dev_name],
                    ["Type", sysinfo.hw_type],
                    ["Model", sysinfo.model],
                    ["Version", sysinfo.sw_ver],
                    ["Signal", format!("{} dB", sysinfo.rssi)],
                    ["Latitude", lat],
                    ["Longitude", lon],
                    ["Mode", sysinfo.active_mode],
                    ["On?", device_is_on(device)],
                ])
            },
            Format::JSON => {
                let location = device.location().ok();

                json!({
                    "addr": addr,
                    "device": Self::device(device),
                    "data": {
                        "system": sysinfo,
                        "location": location,
                    },
                })
            },
        }
    }

    fn actioned(self, addr: SocketAddr, device: Device, sysinfo: SysInfo, action: &'static str, result: Value) -> Value {
        match self {
            Format::Short => {
                json!([
                    ["Address", addr],
                    ["Alias", sysinfo.alias],
                    ["Product", sysinfo.dev_name],
                    ["Model", sysinfo.model],
                    [action, result],
                ])
            },
            Format::Long => {
                json!([
                    ["Address", addr],
                    ["MAC", sysinfo.mac],
                    ["Alias", sysinfo.alias],
                    ["Product", sysinfo.dev_name],
                    ["Type", sysinfo.hw_type],
                    ["Model", sysinfo.model],
                    ["Version", sysinfo.sw_ver],
                    [action, result],
                ])
            },
            Format::JSON => json!({
                "addr": addr,
                "actioned": {
                    "action": action,
                    "result": result,
                },
                "device": Self::device(device),
                "data": {
                    "system": sysinfo,
                },
            }),
        }
    }

    fn device(device: Device) -> &'static str {
        match device {
            Device::HS100(_) => "HS100",
            Device::HS110(_) => "HS110",
            Device::LB110(_) => "LB110",
            Device::Unknown(_) => "unknown",
        }
    }
}

fn main() {
    let matches = App::new("TPLink smart device CLI")
        .version("0.1")
        .author("Rob Young <rob@robyoung.digital>")
        .about("Discover and interact with TPLink smart devices on the local network")
        .setting(AppSettings::SubcommandRequiredElseHelp)
        .arg(Arg::with_name("json")
            .long("json")
            .takes_value(false)
            .help("Respond with JSON")
        )
        .arg(Arg::with_name("long")
            .long("long")
            .takes_value(false)
            .help("Display more information")
        )
        .subcommand(SubCommand::with_name("discover")
            .about("Discover devices on the local network")
            .arg(Arg::with_name("timeout")
                 .long("timeout")
                 .takes_value(true)
                 .help("Timeout after (seconds)")
                 .default_value("3")
            )
        )
        .subcommand(SubCommand::with_name("status")
            .about("Given device addresses, return info + status")
            .arg(Arg::with_name("address")
                 .multiple(true)
                 .required(true)
            )
        )
        .subcommand(SubCommand::with_name("reboot")
            .about("Reboot one or more device")
            .arg(Arg::with_name("delay")
                 .long("delay")
                 .takes_value(true)
                 .help("Schedule the reboot (in seconds)")
                 .default_value("1")
            )
            .arg(Arg::with_name("address")
                 .multiple(true)
                 .required(true)
            )
        )
        .get_matches();

    let format = if matches.is_present("json") {
        Format::JSON
    } else if matches.is_present("long") {
        Format::Long
    } else {
        Format::Short
    };

    fn parse_seconds(value: &str, default: u64) -> Duration {
        match value.parse::<u64>() {
            Ok(n) => Duration::from_secs(n),
            Err(_) => Duration::from_secs(default),
        }
    }

    fn parse_addresses(matches: &clap::ArgMatches) -> Vec<SocketAddr> {
        matches.values_of("address").unwrap().into_iter().map(
            |addr| addr.parse().map_err(|_| ()).or_else(|_| -> Result<_, ()> {
                Ok(SocketAddr::new(addr.parse().map_err(|_| ())?, 9999))
            }).expect(&format!("not a valid address: {}", addr))
        ).collect()
    }

    format.output(match matches.subcommand() {
        ("discover", Some(matches)) => {
            let timeout = match matches.value_of("timeout").unwrap() {
                "never" => None,
                value => Some(parse_seconds(value, 3)),
            };

            command_discover(timeout, format)
        },
        ("status", Some(matches)) => {
            command_status(parse_addresses(&matches), format)
        },
        ("reboot", Some(matches)) => {
            command_reboot(parse_addresses(&matches), parse_seconds(matches.value_of("delay").unwrap(), 1), format)
        },
        _ => unreachable!()
    })
}
