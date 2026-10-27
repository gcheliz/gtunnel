# What an MCP client sends, reduced to one shell function for demo-mcp.tape.
#
#   agent tools                     the tool names `gtunnel mcp` offers
#   agent <tool> '<json arguments>' one tools/call, and the text it returns
#
# Each call starts `gtunnel mcp`, performs the initialize handshake, sends one
# request and prints the server's answer through jq. Nothing is rewritten.
agent() {
  init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"agent","version":"0"}}}'
  ready='{"jsonrpc":"2.0","method":"notifications/initialized"}'
  if [ "$1" = tools ]; then
    printf '%s\n%s\n%s\n' "$init" "$ready" \
      '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
      | gtunnel mcp 2>/dev/null \
      | jq -r 'select(.id==2) | .result.tools | "\(length) tools: " + (map(.name) | join("  "))' \
      | fold -s -w 120
  else
    printf '%s\n%s\n{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"%s","arguments":%s}}\n' \
      "$init" "$ready" "$1" "$2" \
      | gtunnel mcp 2>/dev/null \
      | jq -r 'select(.id==2) | .result.content[0].text'
  fi
}
