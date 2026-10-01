#!/bin/sh
# Install c1760 and its CUPS driver, and optionally add a print queue.
#
#   cargo build --release
#   sudo ./install.sh PRINTER_ADDRESS    # e.g. 192.168.1.50
#   sudo ./install.sh                    # files only, no queue
#   sudo ./install.sh --uninstall
set -e
cd "$(dirname "$0")"

QUEUE=C1760nw
SERVERBIN=$(cups-config --serverbin 2>/dev/null || echo /usr/lib/cups)
DATADIR=$(cups-config --datadir 2>/dev/null || echo /usr/share/cups)

if [ "$(id -u)" != 0 ]; then
    echo "Run as root: sudo $0 $*" >&2
    exit 1
fi

if [ "$1" = "--uninstall" ]; then
    lpadmin -x "$QUEUE" 2>/dev/null || true
    rm -f /usr/local/bin/c1760 "$SERVERBIN/filter/c1760-filter" "$DATADIR/model/Dell-C1760nw.ppd"
    echo "Removed c1760 and the $QUEUE queue."
    exit 0
fi

if [ ! -x target/release/c1760 ]; then
    echo "Build first (as yourself, not root): cargo build --release" >&2
    exit 1
fi
command -v gs >/dev/null || echo "Warning: Ghostscript (gs) is not installed; printing needs it." >&2

install -m 755 target/release/c1760 /usr/local/bin/c1760
install -m 755 cups/c1760-filter "$SERVERBIN/filter/c1760-filter"
install -d "$DATADIR/model"
install -m 644 cups/Dell-C1760nw.ppd "$DATADIR/model/Dell-C1760nw.ppd"
echo "Installed /usr/local/bin/c1760, the CUPS filter, and the PPD."

if [ -n "$1" ]; then
    lpadmin -p "$QUEUE" -E -v "socket://$1:9100" -P cups/Dell-C1760nw.ppd \
        -D "Dell C1760nw" -o printer-is-shared=false 2>&1 | grep -v deprecated || true
    if ! lpstat -d 2>/dev/null | grep -q "system default destination:"; then
        lpadmin -d "$QUEUE"
        echo "Made $QUEUE the default printer."
    fi
    echo "Queue $QUEUE ready. Try: lp -d $QUEUE testpage.ps"
fi
