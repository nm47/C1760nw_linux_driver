# Dell C1760nw Linux driver

A Linux printer driver for the Dell C1760nw color laser, which Dell only
supports on Windows. The printer has no PostScript or PCL. It only accepts
HBPL v1, a Fuji Xerox host-based format where the computer rasterizes and
compresses every page. This project implements HBPL v1 in a small Rust
program, `c1760`, and hooks it into CUPS so any application can print.

Other HBPL v1 printers (Dell 1250c and C1660w, Epson AcuLaser C1700,
Fuji Xerox DocuPrint CP105b) should work as well, but only the C1760nw has
been tested.

## Install

You need Rust (`cargo`), Ghostscript, and CUPS. Then:

```sh
cargo build --release
sudo ./install.sh 192.168.1.50     # your printer's IP address
```

This installs `/usr/local/bin/c1760`, a CUPS filter and PPD, and a print
queue named `C1760nw`, which becomes the default printer if you don't have
one. After that, print from any application or with `lp`:

```sh
lp -d C1760nw document.pdf
lp -d C1760nw -o ColorModel=Gray -o PageSize=A4 document.pdf
```

Run `sudo ./install.sh --uninstall` to remove everything.

## Command line

`c1760` also works without CUPS:

```sh
c1760 print document.pdf [--mono] [--paper a4] [--host 192.168.1.50]
c1760 status            # printer state and toner levels over SNMP
c1760 decode job.hbpl --out page   # inspect a job, write pages as images
```

The printer address comes from `--host`, then `$C1760_HOST`, then the CUPS
queue. Run `c1760 help` for all options.

## How it works

The printer takes raw jobs on TCP port 9100. A job is a PJL header followed
by HBPL records, and each page is 600 dpi 8-bit CMYK compressed with a
scheme that copies runs from neighboring pixels. [PROTOCOL.md](PROTOCOL.md)
documents the format.

The encoder's output is byte-identical to Dave Coffin's reference
`foo2hbpl1`. The tests cover encoding and decoding round trips.

```sh
cargo test
lp -d C1760nw testpage.ps   # color, gray, text, and margin check page
```

## Credits

The HBPL v1 format was reverse-engineered by Dave Coffin
([dechifro.org/hbpl](https://dechifro.org/hbpl/)), whose `foo2hbpl1`
this encoder follows. The job header details came from
[HBPLv1.0 Plus](https://github.com/macos2/HBPLv1.0_Plus-Printer-driver).

## License

GPL-2.0-or-later, as for `foo2hbpl1`. See [LICENSE](LICENSE).
