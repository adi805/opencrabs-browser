#!/usr/bin/env python3
"""Sum RSS over a whole process tree. usage: rss.py <root_pid> [label]"""
import os, sys


def children_map():
    kids = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open("/proc/%s/stat" % d) as f:
                parts = f.read().rsplit(") ", 1)[1].split()
            ppid = int(parts[1])
        except Exception:
            continue
        kids.setdefault(ppid, []).append(int(d))
    return kids


def rss_kb(pid):
    try:
        with open("/proc/%d/status" % pid) as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except Exception:
        pass
    return 0


def tree(root, kids):
    out, stack = [], [root]
    while stack:
        p = stack.pop()
        out.append(p)
        stack.extend(kids.get(p, []))
    return out


def find_root(pattern):
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            cmd = open("/proc/%s/cmdline" % d, "rb").read().replace(b"\0", b" ").decode("utf8", "replace")
        except Exception:
            continue
        if pattern in cmd and "remote-debugging-port" in cmd:
            return int(d)
    return None


kids = children_map()
pat = sys.argv[1]
label = sys.argv[2] if len(sys.argv) > 2 else pat
if pat.isdigit():
    root = int(pat)
else:
    root = find_root(pat)
if not root:
    print("no root process matching %r" % pat)
    sys.exit(2)
pids = tree(root, kids)
total = sum(rss_kb(p) for p in pids)
print("%-22s root=%-7d procs=%-3d RSS=%.0f MB" % (label, root, len(pids), total / 1024.0))