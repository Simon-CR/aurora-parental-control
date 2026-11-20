package catalog

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestValidateTitleID(t *testing.T) {
	cases := []struct {
		in     string
		wantID string
		wantOK bool
	}{
		{"45410912", "45410912", true},
		{"584108b7.jpg", "584108B7", true},
		{"c0de9999.JPEG", "C0DE9999", true},
		{"", "", false},
		{"4541091", "", false},
		{"454109123", "", false},
		{"../secret", "", false},
		{"..\\secret", "", false},
		{"4541/912", "", false},
		{"ZZZZZZZZ", "", false},
		{"45410912.png", "", false},
	}
	for _, tc := range cases {
		got, ok := ValidateTitleID(tc.in)
		if ok != tc.wantOK || got != tc.wantID {
			t.Errorf("ValidateTitleID(%q) = (%q, %v), want (%q, %v)", tc.in, got, ok, tc.wantID, tc.wantOK)
		}
	}
}

func TestOpenEmptySaveReloadAndAssets(t *testing.T) {
	dir := t.TempDir()

	s, err := Open(dir)
	if err != nil {
		t.Fatalf("Open empty failed: %v", err)
	}
	tsv, etag := s.CatalogTSV()
	if !strings.HasPrefix(string(tsv), "SHELF_CATALOG\tv1\t0\t0") || etag == "" {
		t.Fatalf("unexpected empty catalog: %q etag=%q", string(tsv), etag)
	}
	st := s.Status()
	if st.GameCount != 0 || st.PosterCount != 0 || st.BackdropCount != 0 {
		t.Fatalf("unexpected empty status: %+v", st)
	}

	// Reject malformed payload.
	if _, err := s.SaveCatalogTSV([]byte("not a catalog")); err == nil {
		t.Fatalf("expected error on missing header")
	}
	if _, err := s.SaveCatalogTSV([]byte("SHELF_CATALOG\tv1\t100\t1\nbad\tline\n")); err == nil {
		t.Fatalf("expected error on short column count")
	}

	sample := strings.Join([]string{
		"SHELF_CATALOG\tv1\t1700000000\t2",
		"12\t45410912\t0E7A7EB3\tPortal 2\t2\t1\t1\tUsb0:\t\\games\\portal 2\tdefault.xex\tValve\t2011\t1\t1\tTest-chamber co-op puzzle game.",
		"102\t584108B7\t00000001\tCastle Crashers\t6\t2\t3\tUsb0:\t\\content\\0000000000000000\\584108b7\\000d0000\tf943\tThe Behemoth\t2008\t1\t0\tSide-scrolling beat-em-up.",
	}, "\n") + "\n"

	st2, err := s.SaveCatalogTSV([]byte(sample))
	if err != nil {
		t.Fatalf("SaveCatalogTSV failed: %v", err)
	}
	if st2.GameCount != 2 || st2.CatalogVersion == "" || st2.UpdatedAt == "" {
		t.Fatalf("unexpected saved status: %+v", st2)
	}

	// Write sample poster and backdrop files.
	if err := os.WriteFile(filepath.Join(dir, "posters", "45410912.jpg"), []byte{0xFF, 0xD8, 0xFF}, 0o644); err != nil {
		t.Fatalf("write poster: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "backdrops", "45410912.jpg"), []byte{0xFF, 0xD8, 0xFF}, 0o644); err != nil {
		t.Fatalf("write backdrop: %v", err)
	}

	pPath, ok := s.PosterPath("45410912.jpg")
	if !ok || !strings.HasSuffix(pPath, "45410912.jpg") {
		t.Fatalf("PosterPath failed: path=%q ok=%v", pPath, ok)
	}
	if _, ok := s.PosterPath("../45410912"); ok {
		t.Fatalf("PosterPath allowed traversal")
	}
	if bPath, ok := s.BackdropPath("45410912"); !ok || !strings.HasSuffix(bPath, "45410912.jpg") {
		t.Fatalf("BackdropPath failed: path=%q ok=%v", bPath, ok)
	}

	// Reload from disk.
	s2, err := Open(dir)
	if err != nil {
		t.Fatalf("reload Open failed: %v", err)
	}
	st3 := s2.Status()
	if st3.GameCount != 2 || st3.PosterCount != 1 || st3.BackdropCount != 1 {
		t.Fatalf("unexpected reloaded status: %+v", st3)
	}
}
