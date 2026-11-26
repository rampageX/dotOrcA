// S00 GUI capability probe; not the production UI.
package main

import (
	"fmt"
	"strings"
	"sync/atomic"
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
	workerStatus := widget.NewLabel("Worker: idle / 后台待命")
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
	var run *widget.Button
	run = widget.NewButton("5s worker test / 后台测试", func() {
		run.Disable()
		workerStatus.SetText("Worker: running (5 seconds)")
		var steps atomic.Int32
		var elapsedMS atomic.Int64
		// Worker never calls or waits for Fyne.
		go func() {
			start := time.Now()
			for i := int32(1); i <= 5; i++ {
				time.Sleep(time.Second)
				if i == 5 {
					elapsedMS.Store(time.Since(start).Milliseconds())
				}
				steps.Store(i)
			}
		}()
		// Separate observer, one outstanding update; read the latest state.
		go func() {
			ticker := time.NewTicker(100 * time.Millisecond)
			defer ticker.Stop()
			for range ticker.C {
				finished := false
				fyne.DoAndWait(func() {
					n := steps.Load()
					if n == 5 {
						workerStatus.SetText(fmt.Sprintf("Worker: DONE 5/5 | actual elapsed: %d ms", elapsedMS.Load()))
						run.Enable()
						finished = true
					} else {
						workerStatus.SetText(fmt.Sprintf("Worker: %d/5", n))
					}
				})
				if finished {
					return
				}
			}
		}()
	})
	clear := widget.NewButton("Clear / 清空", func() { lines = nil; output.SetText(""); status.SetText("Ready / 等待拖放") })
	w.SetContent(container.NewBorder(container.NewVBox(widget.NewLabel("S00 only — no video processing"), status, workerStatus), container.NewHBox(run, clear), nil, nil, output))
	w.Resize(fyne.NewSize(760, 480))
	w.ShowAndRun()
}
