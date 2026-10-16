# Source provenance

Copied from msb-agent-desktop commit 2e09124 (running browser implementation 24849ee), 2026-10-11.
Only guest browser dependencies, script compilation, observation and worker primitives are reused. The standalone MCP server, authentication, shell tool and frontend server are not shipped.
Upstream dependencies remain governed by their package licenses; Playwright, Node, Chromium, LXQt, TigerVNC and fonts are inventoried during image build.
