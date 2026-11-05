out=$1
shift
{
  tr '\0' '\n' < /proc/$PPID/cmdline | sed 's/^/sh_argv=/'
  printf 'argc=%s\n' "$#"
  for a in "$@"; do printf 'arg=%s\n' "$a"; done
} >> "$out"
