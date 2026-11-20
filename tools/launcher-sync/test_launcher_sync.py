import os
import sqlite3
import tempfile
import unittest

import launcher_sync


def make_test_dbs(temp_dir: str) -> tuple[bytes, bytes]:
    c_path = os.path.join(temp_dir, "content.db")
    s_path = os.path.join(temp_dir, "settings.db")

    scon = sqlite3.connect(s_path)
    scon.execute(
        "CREATE TABLE ScanPaths (Id INTEGER PRIMARY KEY, Path TEXT, DeviceId TEXT, Depth INTEGER, ScriptData TEXT, OptionsFlag INTEGER)"
    )
    scon.execute("INSERT INTO ScanPaths VALUES (4, '\\', 'DEV_USB0', 4, '', 1)")
    scon.execute("INSERT INTO ScanPaths VALUES (5, '\\ExtraGames', 'DEV_HDD1', 3, '', 1)")
    scon.commit()
    scon.close()

    con = sqlite3.connect(c_path)
    con.execute(
        "CREATE TABLE MountedDevices (DeviceId TEXT, DeviceName TEXT, VirtualRoot TEXT, MountPoint TEXT)"
    )
    con.execute("INSERT INTO MountedDevices VALUES ('DEV_USB0', 'Usb0', '\\Xbox360\\System\\Usb0', 'Usb0:')")
    con.execute("INSERT INTO MountedDevices VALUES ('DEV_HDD1', 'Hdd1', '\\Xbox360\\System\\Hdd1', 'Hdd1:')")
    con.execute(
        """
        CREATE TABLE ContentItems (
            Id INTEGER PRIMARY KEY,
            Directory TEXT,
            Executable TEXT,
            TitleId INTEGER,
            MediaId INTEGER,
            TitleName TEXT,
            Description TEXT,
            Publisher TEXT,
            ReleaseDate TEXT,
            GenreFlag INTEGER,
            FileType INTEGER,
            ContentGroup INTEGER,
            ScanPathId INTEGER,
            GameCapsOffline INTEGER DEFAULT 0
        )
        """
    )
    rows = [
        (
            12,
            "\\games\\portal 2",
            "default.xex",
            0x45410912,
            0x0E7A7EB3,
            "Portal 2",
            "Line1\nLine2\tTabbed " + ("x" * 260),
            "Electronic Arts",
            "2012-07-10",
            2,
            1,
            1,
            4,
            0x02010201,
        ),
        (
            13,
            "\\games\\skate 3\\454108e6\\00007000",
            "5c087c2c",
            0x454108E6,
            0x5C087C2C,
            "Skate 3",
            "Single Player, Xbox LIVE Multiplayer 1-4.",
            "Electronic Arts",
            "2014-09-23",
            1024,
            3,
            1,
            4,
            0,
        ),
        (
            102,
            "\\content\\0000000000000000\\584108b7\\000d0000",
            "f9432a7fe407a3c196c95d0ffd4a540937fd4eec58",
            0x584108B7,
            0x11223344,
            "Castle Crashers",
            "Co-op arcade brawler.",
            "The Behemoth",
            "2008-08-27",
            6,
            3,
            2,
            4,
            0x04020402,
        ),
        (
            470,
            "\\homebrew\\xexmenu",
            "default.xex",
            -1059153511,  # signed 32-bit 0xC0DE9999
            0,
            "XeXMenu 1.1",
            "Local Multiplayer 1-4 test.",
            "",
            "",
            0,
            1,
            5,
            5,
            0,
        ),
        (
            999,
            "\\skip",
            "none",
            0,
            0,
            "Zero TitleId",
            "",
            "",
            "",
            0,
            1,
            1,
            4,
            0,
        ),
    ]
    con.executemany(
        "INSERT INTO ContentItems VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rows,
    )
    con.commit()
    con.close()

    with open(c_path, "rb") as f:
        c_bytes = f.read()
    with open(s_path, "rb") as f:
        s_bytes = f.read()
    return c_bytes, s_bytes


class LauncherSyncTests(unittest.TestCase):
    def test_load_and_render_catalog_tsv(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            c_bytes, s_bytes = make_test_dbs(td)
            rows = launcher_sync.load_catalog_rows(c_bytes, s_bytes)
            self.assertEqual(len(rows), 4)

            by_tid = {r.title_id: r for r in rows}
            self.assertIn("45410912", by_tid)
            self.assertIn("454108E6", by_tid)
            self.assertIn("584108B7", by_tid)
            self.assertIn("C0DE9999", by_tid)

            portal = by_tid["45410912"]
            self.assertEqual(portal.device, "Usb0:")
            self.assertEqual(portal.directory, "\\games\\portal 2")
            self.assertEqual(portal.executable, "default.xex")
            self.assertEqual(portal.release_year, "2012")
            self.assertEqual(portal.local_players, 2)
            self.assertNotIn("\n", portal.description)
            self.assertNotIn("\t", portal.description)
            self.assertLessEqual(len(portal.description), 240)

            self.assertEqual(by_tid["584108B7"].local_players, 4)
            self.assertEqual(by_tid["454108E6"].local_players, 0)  # Xbox LIVE-only excluded

            xexmenu = by_tid["C0DE9999"]
            self.assertEqual(xexmenu.device, "Hdd1:")
            self.assertEqual(xexmenu.directory, "\\ExtraGames\\homebrew\\xexmenu")
            self.assertEqual(xexmenu.local_players, 4)

            tsv = launcher_sync.render_catalog_tsv(
                rows,
                poster_ids={"45410912", "584108B7"},
                backdrop_ids={"45410912"},
                timestamp=1700000000,
            )
            lines = tsv.strip().split("\n")
            self.assertEqual(len(lines), 5)
            self.assertEqual(lines[0], "SHELF_CATALOG\tv1\t1700000000\t4")

            for row_line in lines[1:]:
                cols = row_line.split("\t")
                self.assertEqual(len(cols), 16)
                if cols[1] == "45410912":
                    self.assertEqual(cols[12], "1")
                    self.assertEqual(cols[13], "1")
                    self.assertEqual(cols[15], "2")
                elif cols[1] == "454108E6":
                    self.assertEqual(cols[12], "0")
                    self.assertEqual(cols[13], "0")
                    self.assertEqual(cols[15], "0")

    def test_detect_local_players_variants(self) -> None:
        self.assertEqual(launcher_sync.detect_local_players(0x02010201, ""), 2)
        self.assertEqual(launcher_sync.detect_local_players(0x04020402, ""), 4)
        self.assertEqual(launcher_sync.detect_local_players(0x00000101, ""), 1)
        self.assertEqual(launcher_sync.detect_local_players(0, "Supports offline co-op!"), 2)
        self.assertEqual(launcher_sync.detect_local_players(0, "Local Multiplayer 1-4."), 4)
        self.assertEqual(launcher_sync.detect_local_players(0, "Single Player, Xbox LIVE Multiplayer 2-4"), 0)

    def test_merge_scan_directory(self) -> None:
        self.assertEqual(launcher_sync.merge_scan_directory("\\", "\\games\\a"), "\\games\\a")
        self.assertEqual(
            launcher_sync.merge_scan_directory("\\Games", "\\Games\\Portal 2"),
            "\\Games\\Portal 2",
        )
        self.assertEqual(
            launcher_sync.merge_scan_directory("\\Games\\", "\\Portal 2"),
            "\\Games\\Portal 2",
        )


if __name__ == "__main__":
    unittest.main()
