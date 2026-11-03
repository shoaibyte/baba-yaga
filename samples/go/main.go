// Command sample uses old golang.org/x/text and golang.org/x/net releases
// on purpose, so osv-scanner has known vulnerabilities to report.
package main

import (
	"fmt"

	"golang.org/x/net/html"
	"golang.org/x/text/language"
)

func main() {
	tag, err := language.Parse("nb-NO")
	if err != nil {
		panic(err)
	}
	fmt.Println(tag, html.EscapeString("<b>sample</b>"))
}
