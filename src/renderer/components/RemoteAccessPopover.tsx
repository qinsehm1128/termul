import { AlertCircle, Check, Copy, Monitor, ShieldAlert } from 'lucide-react'
import { QRCodeSVG } from 'qrcode.react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { Switch } from '@/components/ui/switch'
import { toProjectGroupSummaries, toProjectSummaries } from '@/hooks/use-projects-persistence'
import { useSshTranslation } from '@/hooks/use-ssh-translation'
import { remoteServerApi, syncProjects } from '@/lib/api'
import { cn } from '@/lib/utils'
import { useProjectStore } from '@/stores/project-store'
import { useRemoteStatus, useRemoteStatusStore } from '@/stores/remote-status-store'

const statusBarTriggerClass =
  'flex h-5 cursor-pointer items-center rounded-sm px-1.5 text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground'

/**
 * StatusBar popover for remote agent access.
 *
 * Enabling starts the in-process localhost web server + a built-in cloudflared
 * quick-tunnel, producing an ephemeral `https://*.trycloudflare.com` URL the
 * phone can reach on any network. That URL is rendered as a QR — scan to open.
 * No bind selector, no URL text row: the QR is the connect UI.
 *
 * The credential is carried only inside the access URL fragment used by the QR/copy controls.
 * It is never rendered as standalone text; the browser consumes and clears the fragment on load.
 */
export function RemoteAccessPopover(): React.JSX.Element {
  const t = useSshTranslation()
  const remoteStatus = useRemoteStatus()
  const [remoteBusy, setRemoteBusy] = useState(false)
  const [remoteError, setRemoteError] = useState<string | null>(null)
  const [copiedUrl, setCopiedUrl] = useState(false)

  const isRunning = remoteStatus?.running ?? false
  // Trusted local Desktop host state is the only credential source. Revoked WebSocket payloads
  // are never accepted by this component, and an uncredentialed tunnel URL is never promoted.
  const accessUrl = remoteStatus?.accessUrl ?? null
  // Track whether a tunnel URL was ever seen this session so the popover can
  // distinguish "Starting tunnel…" (never connected) from "Tunnel
  // disconnected" (was connected, now gone — the 3s status poll cleared it).
  const [sawUrl, setSawUrl] = useState(false)
  useEffect(() => {
    if (accessUrl) setSawUrl(true)
    if (!isRunning) setSawUrl(false)
  }, [accessUrl, isRunning])

  const handleRemoteToggle = async (enable: boolean): Promise<void> => {
    setRemoteBusy(true)
    setRemoteError(null)
    try {
      const result = enable ? await remoteServerApi.start() : await remoteServerApi.stop()
      if (result.success) {
        useRemoteStatusStore.getState().setStatus(result.data)
        // Epic-4 bridge: seed the in-memory project registry so the web/remote
        // client sees the desktop's project list immediately (the live-push
        // path in `useProjectsAutoSave` keeps it in sync on later mutations).
        // No env-var values cross the wire — redact-by-omission.
        if (enable) {
          const { projects, groups, activeProjectId } = useProjectStore.getState()
          // Await + inspect: a failed seed leaves the web client without a
          // project list until the next desktop mutation re-syncs — surface it.
          const syncResult = await syncProjects(
            toProjectSummaries(projects, activeProjectId),
            activeProjectId || null,
            toProjectGroupSummaries(groups)
          )
          if (!syncResult.success) {
            toast.error(t('remote.seedProjectsFailed', { error: syncResult.error }))
          }
          // Desktop-hosted browser history reads the durable Rust provider
          // directly, so no renderer transcript seed is needed.
        }
      } else {
        setRemoteError(result.error)
      }
    } catch (error) {
      setRemoteError(error instanceof Error ? error.message : String(error))
    } finally {
      setRemoteBusy(false)
    }
  }

  const handleCopyLink = async (): Promise<void> => {
    if (!accessUrl) return
    try {
      await navigator.clipboard.writeText(accessUrl)
      setCopiedUrl(true)
      setTimeout(() => setCopiedUrl(false), 1500)
    } catch {
      // Clipboard unavailable; ignore.
    }
  }

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={statusBarTriggerClass}
          aria-label={t('remote.aria')}
          aria-pressed={isRunning}
        >
          <Monitor size={14} className={cn('mr-0', isRunning ? 'text-green-300' : undefined)} />
          {isRunning && <span className="sr-only">{t('remote.enabled')}</span>}
        </button>
      </PopoverTrigger>
      <PopoverContent side="top" align="end" className="w-72 p-4">
        <div className="space-y-3">
          <div>
            <h4 className="font-medium text-sm text-foreground">{t('remote.title')}</h4>
            <p className="text-xs text-muted-foreground mt-1">{t('remote.description')}</p>
          </div>

          <div className="flex items-center justify-between gap-3">
            <div className="flex-1 min-w-0">
              <div className="text-sm text-foreground">{t('remote.enable')}</div>
              <div className="text-xs text-muted-foreground mt-0.5">{t('remote.enableHint')}</div>
            </div>
            <Switch
              checked={isRunning}
              disabled={remoteBusy}
              onCheckedChange={(checked) => void handleRemoteToggle(checked)}
              aria-label={t('remote.toggleAria')}
            />
          </div>

          {remoteError && (
            <div className="flex items-start gap-2 text-sm text-destructive bg-destructive/10 border border-destructive/30 rounded-md px-3 py-2">
              <AlertCircle className="w-4 h-4 mt-0.5 shrink-0" />
              <span>{remoteError}</span>
            </div>
          )}

          {isRunning && accessUrl && (
            <div className="space-y-2">
              {/* White pad so the black QR modules are legible in dark themes. */}
              <div className="flex justify-center">
                <div className="rounded-lg bg-white p-2">
                  <QRCodeSVG value={accessUrl} size={160} level="M" />
                </div>
              </div>
              <p className="text-xs text-muted-foreground">
                {remoteStatus?.tunnelProvider === 'cloudflareNamed'
                  ? t('remote.providerNamed')
                  : remoteStatus?.tunnelProvider === 'frp'
                    ? t('remote.providerFrp')
                    : t('remote.providerQuick')}
              </p>
              <div className="flex items-start gap-2 text-xs text-amber-600 dark:text-amber-400 bg-amber-500/10 border border-amber-500/30 rounded-md px-3 py-2">
                <ShieldAlert className="w-4 h-4 mt-0.5 shrink-0" />
                <span>{t('remote.securityWarning')}</span>
              </div>
              <button
                type="button"
                onClick={() => void handleCopyLink()}
                className="w-full inline-flex items-center justify-center gap-2 text-xs bg-secondary hover:bg-secondary/80 border border-border rounded-md px-3 py-1.5 transition-colors"
                aria-label={t('remote.copyAria')}
              >
                {copiedUrl ? <Check className="w-3.5 h-3.5" /> : <Copy className="w-3.5 h-3.5" />}
                {copiedUrl ? t('remote.copied') : t('remote.copy')}
              </button>
            </div>
          )}

          {isRunning && !accessUrl && (
            <div className="flex items-center justify-center text-xs text-muted-foreground py-2">
              {sawUrl ? t('remote.disconnected') : t('remote.starting')}
            </div>
          )}
        </div>
      </PopoverContent>
    </Popover>
  )
}
