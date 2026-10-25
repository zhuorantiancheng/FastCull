# FastCull's private, persistent Photoshop COM worker. Never changes Adobe preferences.
# Requests are JSON string literals containing ExtendScript; responses are one line.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$photoshop = $null
# All editor processes share the same Photoshop host. The lock lives here,
# around the actual COM call, so a cancelled Rust waiter cannot release it early.
$hostMutex = [System.Threading.Mutex]::new($false, 'Local\FastCull.AdobeRawRenderer.v2')
[Console]::WriteLine('READY')
[Console]::Out.Flush()
while ($null -ne ($request = [Console]::ReadLine())) {
    $locked = $false
    $lease = $null
    try {
        $message = ConvertFrom-Json -InputObject $request
        $javascript = $message.script
        if ($javascript -isnot [string]) { throw 'Invalid Photoshop worker request.' }
        $cancel = [string]$message.cancel
        if ($message.lease) {
            # Survives the editor process exiting while Camera Raw is inside a
            # synchronous open. Other editors' cache sweep must keep this copy.
            $lease = [System.IO.File]::Open([string]$message.lease, [System.IO.FileMode]::OpenOrCreate, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
        }
        $started = [System.Diagnostics.Stopwatch]::StartNew()
        while (-not $locked) {
            if ($cancel -and [System.IO.File]::Exists($cancel)) { throw 'Adobe RAW preview cancelled before starting.' }
            try { $locked = $hostMutex.WaitOne(100) }
            catch [System.Threading.AbandonedMutexException] {
                $locked = $true
                throw 'A previous Adobe worker was interrupted. Photoshop may still be processing it; retry after it finishes.'
            }
            if (-not $locked -and $started.ElapsedMilliseconds -ge 5000) { throw 'Another RAW editor is using Photoshop. Retry when its render finishes.' }
        }
        if ($cancel -and [System.IO.File]::Exists($cancel)) { throw 'Adobe RAW preview cancelled before starting.' }
        if ($null -eq $photoshop) {
            try { $photoshop = New-Object -ComObject Photoshop.Application }
            catch { throw "Cannot connect to Adobe Photoshop / Camera Raw. Install Photoshop with Camera Raw, finish any startup dialog, and retry. $($_.Exception.Message)" }
        }
        $result = $photoshop.DoJavaScript($javascript)
        [Console]::WriteLine([string]$result)
    }
    catch {
        $message = $_.Exception.Message -replace '[\r\n\t]', ' '
        [Console]::WriteLine("ERR`t$message")
    }
    finally {
        if ($locked) { $hostMutex.ReleaseMutex() }
        if ($null -ne $lease) { $lease.Dispose() }
    }
    [Console]::Out.Flush()
}
$hostMutex.Dispose()
# Releasing our COM reference does not close any user documents or quit Photoshop.
if ($null -ne $photoshop) {
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($photoshop)
}
