mod bits;
mod decode;
mod hbpl;
mod printer;
mod raster;

use hbpl::{Clip, Paper, PAPERS};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

const USAGE: &str = "\
c1760 - a tiny driver for the Dell C1760nw and other HBPL v1 printers

Usage:
  c1760 print FILE [--mono] [--paper NAME] [--media N] [--host H] [-o OUT]
      Render a PDF or PostScript file with Ghostscript, encode it, and send
      it to the printer (or write the job to OUT; - for stdout).
  c1760 status [--host H]
      Show printer state and toner levels.
  c1760 send JOB [--host H]
      Send an already encoded job.
  c1760 encode [--media N] [--clip L,T,R,B] [-J NAME] [-U USER] < PNM > JOB
      Encode Ghostscript raster output (pbmraw, pgmraw, ppmraw, or
      pamcmyk32; 600 dpi) from stdin. Drop-in for foo2hbpl1.
  c1760 decode JOB [--out BASE]
      Dump a job's structure; with --out, also write each page as
      BASE-NN.ppm (color) or BASE-NN.pgm (mono).

The printer is --host, else $C1760_HOST, else the address of a CUPS queue
with 1760 in its name (as set up by install.sh).
Papers: letter (default), legal, a4, executive, a5, b5, folio, env10,
        monarch, c5, dl.
Media:  1 plain, 2 bond, 3 lwcard, 4 lwgcard, 5 labels, 6 envelope,
        7 recycled, 8-12 second side of 1, 2, 3, 4, 7.
";

struct Opts {
    args: Vec<String>,
    host: Option<String>,
    mono: bool,
    paper: &'static Paper,
    media: Option<u8>,
    out: Option<String>,
    clip: Clip,
    job_name: String,
    user: String,
}

fn parse_opts(raw: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        args: Vec::new(),
        host: None,
        mono: false,
        paper: &PAPERS[0],
        media: None,
        out: None,
        clip: Clip::default(),
        job_name: String::new(),
        user: String::new(),
    };
    let mut it = raw.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--host" => o.host = Some(val()?),
            "--mono" => o.mono = true,
            "--paper" => {
                let name = val()?.to_lowercase();
                o.paper = PAPERS.iter().find(|p| p.name == name).ok_or(format!("unknown paper {name}"))?;
            }
            "--media" | "-m" => {
                let m: u8 = val()?.parse().map_err(|_| "bad media number")?;
                if !(1..=hbpl::MEDIA_NAMES.len() as u8).contains(&m) {
                    return Err(format!("media must be 1-{}", hbpl::MEDIA_NAMES.len()));
                }
                o.media = Some(m);
            }
            "-o" | "--out" => o.out = Some(val()?),
            "--clip" | "-u" => {
                let v: Vec<usize> = val()?.split(',').filter_map(|x| x.parse().ok()).collect();
                o.clip = Clip(v.try_into().map_err(|_| "--clip needs L,T,R,B")?);
            }
            "-J" => o.job_name = val()?,
            "-U" => o.user = val()?,
            _ if a.starts_with('-') && a.len() > 1 => return Err(format!("unknown option {a}")),
            _ => o.args.push(a.clone()),
        }
    }
    Ok(o)
}

impl Opts {
    fn host(&self) -> Result<String, String> {
        if let Some(h) = self.host.clone().or_else(|| env::var("C1760_HOST").ok()) {
            return Ok(h);
        }
        // Fall back to the CUPS queue, e.g. "device for C1760nw: socket://10.0.0.5:9100".
        let lpstat = Command::new("lpstat").arg("-v").output().map(|o| o.stdout).unwrap_or_default();
        String::from_utf8_lossy(&lpstat)
            .lines()
            .filter(|l| l.split(':').next().unwrap_or("").contains("1760"))
            .find_map(|l| l.split("socket://").nth(1))
            .and_then(|rest| rest.split([':', '/']).next())
            .map(str::to_string)
            .ok_or_else(|| "no printer given: use --host, set C1760_HOST, or run install.sh".into())
    }
}

/// UTC date and time for the job's (informational) PJL comments.
fn now_strings() -> (String, String) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (days, t) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    (format!("{m:02}/{d:02}/{y}"), format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60))
}

/// Encode every page on `input` into a complete job. Returns the job and
/// the page count.
fn encode_stream(input: &mut impl BufRead, o: &Opts) -> io::Result<(Vec<u8>, u32)> {
    let mut job = Vec::new();
    let mut pages = 0;
    while let Some(r) = raster::read_page(input)? {
        if pages == 0 {
            let (date, time) = now_strings();
            let hostname = fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
            job.extend(hbpl::job_header(&hbpl::Job {
                color: r.color,
                media: o.media.unwrap_or_else(|| hbpl::default_media(r.width, r.height)),
                filename: o.job_name.clone(),
                username: o.user.clone(),
                hostname: hostname.trim().to_string(),
                date,
                time,
            }));
        }
        pages += 1;
        job.extend(hbpl::encode_page(&r, pages, o.clip));
    }
    job.extend(hbpl::job_footer());
    Ok((job, pages))
}

fn cmd_print(mut o: Opts) -> Result<(), String> {
    let [file] = o.args.as_slice() else { return Err("print needs exactly one FILE".into()) };
    let file = file.clone();
    if o.job_name.is_empty() {
        o.job_name = file.rsplit('/').next().unwrap_or(&file).to_string();
    }
    if o.user.is_empty() {
        o.user = env::var("USER").unwrap_or_default();
    }

    let mut gs = Command::new("gs")
        .args(["-q", "-dBATCH", "-dNOPAUSE", "-dSAFER", "-sstdout=%stderr"])
        .arg(if o.mono { "-sDEVICE=pgmraw" } else { "-sDEVICE=ppmraw" })
        .arg("-r600")
        .arg(format!("-g{}x{}", o.paper.width, o.paper.height))
        .args(["-dFIXEDMEDIA", "-dPDFFitPage", "-dPSFitPage", "-sOutputFile=-", "-f"])
        .arg(&file)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't run gs: {e}"))?;
    let mut raster = BufReader::with_capacity(1 << 20, gs.stdout.take().unwrap());
    let encoded = encode_stream(&mut raster, &o);
    // Drain so gs isn't stuck on a full pipe if we bailed early.
    io::copy(&mut raster, &mut io::sink()).ok();
    let gs_ok = gs.wait().map_err(|e| e.to_string())?.success();
    let (job, pages) = encoded.map_err(|e| format!("reading gs output: {e}"))?;
    if !gs_ok || pages == 0 {
        return Err(format!("Ghostscript failed to render {file}"));
    }

    match o.out.as_deref() {
        Some("-") => io::stdout().lock().write_all(&job).map_err(|e| e.to_string())?,
        Some(path) => fs::write(path, &job).map_err(|e| format!("{path}: {e}"))?,
        None => {
            let host = o.host()?;
            printer::send_job(&host, &job).map_err(|e| format!("sending to {host}: {e}"))?
        }
    }
    eprintln!(
        "{} {pages} page(s), {} KB, {}",
        if o.out.is_some() { "wrote" } else { "sent" },
        job.len() / 1024,
        if o.mono { "mono" } else { "color" }
    );
    Ok(())
}

fn cmd_status(o: Opts) -> Result<(), String> {
    let host = o.host()?;
    let s = printer::status(&host).map_err(|e| format!("{host}: {e}"))?;
    println!("{}", s.model);
    println!("State: {} ({})", s.state, s.display);
    for sup in &s.supplies {
        match sup.percent {
            Some(p) => {
                let filled = (p.clamp(0, 100) / 5) as usize;
                println!("  {:<18} {:>3}%  {}{}", sup.name, p, "#".repeat(filled), ".".repeat(20 - filled));
            }
            None => println!("  {:<18}    ?", sup.name),
        }
    }
    Ok(())
}

fn cmd_send(o: Opts) -> Result<(), String> {
    let [file] = o.args.as_slice() else { return Err("send needs exactly one JOB file".into()) };
    let job = fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    let host = o.host()?;
    printer::send_job(&host, &job).map_err(|e| format!("sending to {host}: {e}"))
}

fn cmd_encode(o: Opts) -> Result<(), String> {
    let mut input = BufReader::with_capacity(1 << 20, io::stdin().lock());
    let (job, _) = encode_stream(&mut input, &o).map_err(|e| e.to_string())?;
    io::stdout().lock().write_all(&job).map_err(|e| e.to_string())
}

fn cmd_decode(o: Opts) -> Result<(), String> {
    let data = match o.args.as_slice() {
        [file] => fs::read(file).map_err(|e| format!("{file}: {e}"))?,
        [] => {
            let mut v = Vec::new();
            io::stdin().read_to_end(&mut v).map_err(|e| e.to_string())?;
            v
        }
        _ => return Err("decode takes one JOB file".into()),
    };
    let d = decode::decode(&data)?;
    print!("{}", d.log);
    if let Some(base) = &o.out {
        for p in &d.pages {
            let path = format!("{base}-{:02}.{}", p.number, if p.color { "ppm" } else { "pgm" });
            fs::write(&path, p.to_pnm()).map_err(|e| format!("{path}: {e}"))?;
            println!("wrote {path}");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let result = parse_opts(&args[1..]).and_then(|o| match cmd.as_str() {
        "print" => cmd_print(o),
        "status" => cmd_status(o),
        "send" => cmd_send(o),
        "encode" => cmd_encode(o),
        "decode" => cmd_decode(o),
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        _ => Err(format!("unknown command {cmd}\n\n{USAGE}")),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("c1760: {e}");
            ExitCode::FAILURE
        }
    }
}
