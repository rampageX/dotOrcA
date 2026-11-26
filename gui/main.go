// S00 GUI capability probe; not the production UI.
package main

import (
	"fmt"
	"strings"
	"time"

	"fyne.io/fyne/v2"
	"fyne.io/fyne/v2/app"
	"fyne.io/fyne/v2/container"
	"fyne.io/fyne/v2/theme"
	"fyne.io/fyne/v2/widget"
)

func main() {
	a := app.New()
	a.Settings().SetTheme(theme.DarkTheme())
	w := a.NewWindow("dotOrcA | S00 GUI probe")
	status := widget.NewLabel("Ready / 等待拖放")
	output := widget.NewMultiLineEntry()
	output.SetPlaceHolder("Drop files here / 拖入文件")
	var lines []string
	w.SetOnDropped(func(_ fyne.Position, uris []fyne.URI) {
		for _, u := range uris {
			lines = append(lines, fmt.Sprintf("URI: %s\nPath: %s", u.String(), u.Path()))
		}
		output.SetText(strings.Join(lines, "\n\n"))
		status.SetText(fmt.Sprintf("Received %d item(s)", len(lines)))
	})
	run := widget.NewButton("Background UI test / 后台测试", func() {
		go func() {
			for i := 1; i <= 5; i++ {
				time.Sleep(200 * time.Millisecond)
				n := i
				fyne.Do(func() { status.SetText(fmt.Sprintf("Background step %d/5", n)) })
			}
		}()
	})
	clear := widget.NewButton("Clear / 清空", func() { lines = nil; output.SetText(""); status.SetText("Ready / 等待拖放") })
	w.SetContent(container.NewBorder(container.NewVBox(widget.NewLabel("S00 only — no video processing"), status), container.NewHBox(run, clear), nil, nil, output))
	w.Resize(fyne.NewSize(760, 480))
	w.ShowAndRun()
}
