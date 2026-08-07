# THOR judge-transport wrapper for a LOCAL model server (Ollama or LM
# Studio) - see JUDGE-TRANSPORT.md ("Local model (Ollama or LM Studio)")
# for the full walkthrough written for someone who has never done this
# before. This is the "any external command" half of THOR's judge transport
# (serve/src/judge.rs): THOR spawns this script, writes the full judge
# prompt to its stdin, and reads whatever this script prints to stdout as
# the judge's raw answer (which judge::parse_judge_output then parses).
#
# UNVERIFIED ON THIS MACHINE: neither Ollama nor LM Studio is installed
# here, so the HTTP call this script makes has never been run against a
# real local model. What WAS verified here: reading the judge prompt back
# out of stdin exactly the way THOR's transport feeds it in (piped stdin,
# closed after the write) - a standalone one-line version of the
# "[Console]::In.ReadToEnd()" call below was tested directly on this
# machine and correctly captured piped input. Only the model call itself
# is unverified, not the plumbing around it.
#
# What you need to install first (pick ONE):
#   - Ollama (this script's default): https://ollama.com/download
#     After installing, pull a small model once, from an ordinary terminal:
#         ollama pull llama3.2
#     Ollama then listens on http://localhost:11434 automatically whenever
#     it is running (it starts its own background service on install).
#   - LM Studio: https://lmstudio.ai
#     Download a small model from within LM Studio's own "Discover" tab,
#     then open its "Developer" tab and click "Start Server". LM Studio
#     listens on http://localhost:1234 by default while that server is on.
#
# This script itself needs NOTHING new installed - PowerShell ships with
# every supported version of Windows, and Invoke-RestMethod is a built-in
# cmdlet (Microsoft.PowerShell.Utility), confirmed present on this machine.
#
# Example guard-judge-config.json entry that runs this script (see
# guard-judge-config-local-model.example.json at the repo root for the
# full worked example, and edit the "-File" path below to wherever you
# actually saved this script):
#
#   {
#     "command": "powershell.exe",
#     "args": ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
#              "C:\\path\\to\\guard-judge-local-model-wrapper.ps1",
#              "-Backend", "ollama", "-Model", "llama3.2"],
#     "timeout_ms": 15000
#   }
#
# How to tell whether it works, from a plain terminal, BEFORE pointing a
# real THOR session at it (see JUDGE-TRANSPORT.md for the full version):
#   echo hello | powershell -NoProfile -File guard-judge-local-model-wrapper.ps1
# With Ollama actually running and the model actually pulled, this should
# print a few lines of model-generated text. If it prints nothing, or
# PowerShell reports an error, the local server is not reachable yet - fix
# that (start Ollama / LM Studio's server, confirm the model name) before
# copying the config into place.

param(
    # Which local runtime to talk to. "ollama" (default) calls Ollama's
    # own /api/generate endpoint; "lmstudio" calls LM Studio's
    # OpenAI-compatible /v1/chat/completions endpoint instead. Both are
    # plain local HTTP, no API key, nothing leaves the machine.
    [ValidateSet("ollama", "lmstudio")]
    [string]$Backend = "ollama",

    # The model name as your local runtime knows it (e.g. what you ran
    # "ollama pull" with, or the model loaded in LM Studio's own UI).
    [string]$Model = "llama3.2",

    [string]$OllamaUrl = "http://localhost:11434/api/generate",
    [string]$LmStudioUrl = "http://localhost:1234/v1/chat/completions",

    # A client-side safety net independent of THOR's own timeout_ms (which
    # kills THIS WHOLE SCRIPT'S process from the Rust side regardless) - if
    # the local server hangs, this script does not itself hang forever
    # either.
    [int]$TimeoutSec = 30
)

# Read the whole judge prompt from stdin (JUDGE_PROMPT plus the one owner
# prompt to judge - see judge::build_judge_stdin in serve/src/judge.rs).
# Plain UTF-8 text, no other framing.
$prompt = [Console]::In.ReadToEnd()

try {
    if ($Backend -eq "ollama") {
        $body = @{
            model  = $Model
            prompt = $prompt
            stream = $false
        } | ConvertTo-Json -Compress -Depth 5

        $response = Invoke-RestMethod -Uri $OllamaUrl -Method Post -Body $body -ContentType "application/json" -TimeoutSec $TimeoutSec
        Write-Output $response.response
    }
    else {
        $body = @{
            model    = $Model
            messages = @(@{ role = "user"; content = $prompt })
            stream   = $false
        } | ConvertTo-Json -Compress -Depth 5

        $response = Invoke-RestMethod -Uri $LmStudioUrl -Method Post -Body $body -ContentType "application/json" -TimeoutSec $TimeoutSec
        Write-Output $response.choices[0].message.content
    }
}
catch {
    # Fail open, same doctrine as the rest of the judge transport
    # (JUDGE-TRANSPORT.md's "fallback ladder"): if the local server is not
    # running, the model name is wrong, or the answer shape does not match
    # what this script expects, print nothing rather than raising past this
    # script. An empty/absent answer is "no verdict" to
    # judge::parse_judge_output - never a crash, never treated as a block.
    exit 0
}
