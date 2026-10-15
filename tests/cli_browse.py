import json
import re
import unittest
from cli_transactions import Fixture, remaining
from conpty import Terminal

class Browse(unittest.TestCase):
    def test_requires_terminal_and_rejects_json(self):
        with Fixture([200]) as f:
            r=f.run("browse","--json")
            self.assertEqual(r.returncode,1)
            self.assertEqual(json.loads(r.stdout)["error"]["code"],"invalid_input")
            self.assertEqual(f.run("browse").returncode,1)
            self.assertFalse(any(m=="PUT" for m,_ in f.requests))

    def test_real_terminal_default_cancel_then_explicit_confirmation(self):
        with Fixture([200]) as f, Terminal(f.command("browse"),f.environment()) as tty:
            text=tty.expect("browse>")
            target=re.search(r"id=([0-9a-f]{12})  target",text).group(1)
            tty.send(f"detail {target}\r");tty.expect("browse>")
            tty.send(f"use {target}\r");tty.expect("confirm [y/N]>")
            self.assertFalse(any(m=="PUT" for m,_ in f.requests))
            tty.send("\r");tty.expect("browse>")
            self.assertEqual(f.selected,"original")
            tty.send(f"use {target}\r");tty.expect("confirm [y/N]>");tty.send("yes\r");tty.expect("browse>")
            self.assertEqual(f.selected,"target")
            tty.send("q\r");self.assertEqual(tty.wait(),0)

    def test_manual_change_invalidates_confirmation_and_old_ids(self):
        with Fixture([200]) as f, Terminal(f.command("browse"),f.environment()) as tty:
            target=re.search(r"id=([0-9a-f]{12})  target",tty.expect("browse>")).group(1)
            tty.send(f"use {target}\r");tty.expect("confirm [y/N]>")
            f.selected="target"
            tty.send("yes\r");tty.expect("browse>")
            self.assertFalse(any(m=="PUT" for m,_ in f.requests))
            tty.send(f"use {target}\r");tty.expect("browse>")
            self.assertFalse(any(m=="PUT" for m,_ in f.requests))
            tty.send("q\r");self.assertEqual(tty.wait(),0)

    def test_dry_run_filter_paging_and_menu_cancel(self):
        with Fixture([200]) as f, Terminal(f.command("browse","--dry-run"),f.environment()) as tty:
            target=re.search(r"id=([0-9a-f]{12})  target",tty.expect("browse>")).group(1)
            for command in ["filter target","protocol shadowsocks","line all","rate all","next","prev","refresh"]:
                tty.send(command+"\r");tty.expect("browse>")
            tty.send(f"use {target}\r");tty.expect("confirm [y/N]>");tty.send("y\r");tty.expect("browse>")
            self.assertFalse(any(m=="PUT" for m,_ in f.requests))
            self.assertEqual(f.proxy_requests,0)
            tty.send("\x03");self.assertEqual(tty.wait(),130)

if __name__=="__main__": unittest.main(argv=[__file__,*remaining])
