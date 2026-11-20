package webui

import (
	"embed"
	"io/fs"
	"net/http"
)

//go:embed index.html
var assets embed.FS

// Handler serves the embedded Grounded-Earth web dashboard.
func Handler() http.Handler {
	sub, err := fs.Sub(assets, ".")
	if err != nil {
		panic(err)
	}
	return http.FileServer(http.FS(sub))
}
