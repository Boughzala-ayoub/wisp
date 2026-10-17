# Prints text that exercises the renderer: ligatures, fallback fonts, emoji,
# underline styles and box drawing. Code points instead of literals keep this
# file plain ASCII, so Windows PowerShell 5.1 reads it correctly.
$e = [char]27
function U([int[]]$codes) { -join ($codes | ForEach-Object { [char]::ConvertFromUtf32($_) }) }

"Ligatures:  => -> <- != === !== <= >= :: |> <| www ** ### && || ..."
"Emoji:      $(U 0x1F600,0x20,0x1F389,0x20,0x1F680,0x20,0x2728,0x20,0x1F44D)   CJK: $(U 0x4E2D,0x6587) $(U 0x65E5,0x672C,0x8A9E) $(U 0xD55C,0xAD6D,0xC5B4)"
"Underline:  $e[4mplain$e[0m $e[4:2mdouble$e[0m $e[4:3mcurly$e[0m $e[4:4mdotted$e[0m $e[4:5mdashed$e[0m $e[58:2::255:100:100m$e[4:3mred curly$e[0m $e[9mstrike$e[0m"
"Styles:     $e[1mbold$e[0m $e[3mitalic$e[0m $e[1;3mbold italic$e[0m $e[2mfaint$e[0m $e[7minverse$e[0m"
"Truecolor:  " + (-join (0..47 | ForEach-Object { $r = [int](255 * $_ / 47); "$e[48;2;$r;100;$(255 - $r)m $e[0m" }))
"Box:        $(U 0x256D)$((U 0x2500) * 10)$(U 0x256E)  $(U 0x250C)$((U 0x2500) * 4)$(U 0x252C)$((U 0x2500) * 4)$(U 0x2510)  $(U 0x2588,0x2593,0x2592,0x2591) $(U 0x2580,0x2584,0x258C,0x2590)"
"            $(U 0x2502) Claude   $(U 0x2502)  $(U 0x2502)    $(U 0x2502)    $(U 0x2502)"
"            $(U 0x2570)$((U 0x2500) * 10)$(U 0x256F)  $(U 0x2514)$((U 0x2500) * 4)$(U 0x2534)$((U 0x2500) * 4)$(U 0x2518)"
