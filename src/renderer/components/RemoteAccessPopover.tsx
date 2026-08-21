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
          <Monitor size={14} className={cn('mr-0', isRunning ? 'text-connection' : undefined)} />
          {isRunning && <span className="sr-only">{t('remote.enabled')}</span>}
        </button>
      </PopoverTrigger>
      <PopoverContent
        side="top"
        align="end"
        className="w-72 p-3 shadow-[0_8px_24px_hsl(var(--background)/0.55),inset_0_1px_0_0_hsl(var(--foreground)/0.05)]"
      >
        <div className="space-y-2.5">
          <div>
            <h4 className="text-xs font-medium tracking-[-0.01em] text-foreground">
              {t('remote.title')}
            </h4>
            <p className="mt-1 text-2xs leading-relaxed text-muted-foreground">
              {t('remote.description')}
            </p>
          </div>

          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0 flex-1">
              <div className="text-xs text-foreground">{t('remote.enable')}</div>
              <div className="mt-0.5 text-2xs text-muted-foreground">{t('remote.enableHint')}</div>
            </div>
            <Switch
              checked={isRunning}
              disabled={remoteBusy}
              onCheckedChange={(checked) => void handleRemoteToggle(checked)}
              aria-label={t('remote.toggleAria')}
            />
          </div>

          {remoteError && (
            <div className="flex items-start gap-2 rounded-md border border-destructive/35 bg-destructive/10 px-2.5 py-2 text-xs text-destructive">
              <AlertCircle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span>{remoteError}</span>
            </div>
          )}

          {isRunning && accessUrl && (
            <div className="space-y-2">
              {/* White pad so the black QR modules are legible in dark themes. */}
              <div className="flex justify-center">
                <div className="rounded-md bg-white p-1.5">
                  <QRCodeSVG value={accessUrl} size={160} level="M" />
                </div>
              </div>
              <p className="text-2xs text-muted-foreground">
                {remoteStatus?.tunnelProvider === 'cloudflareNamed'
                  ? t('remote.providerNamed')
                  : remoteStatus?.tunnelProvider === 'frp'
                    ? t('remote.providerFrp')
                    : t('remote.providerQuick')}
              </p>
              <div className="flex items-start gap-2 rounded-md border border-warning/35 bg-warning/10 px-2.5 py-2 text-2xs text-warning">
                <ShieldAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                <span>{t('remote.securityWarning')}</span>
              </div>
              <button
                type="button"
                onClick={() => void handleCopyLink()}
                className="inline-flex h-7 w-full items-center justify-center gap-1.5 rounded-md border border-border/80 bg-secondary/50 px-2.5 text-2xs font-medium text-foreground transition-colors hover:bg-secondary focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                aria-label={t('remote.copyAria')}
              >
                {copiedUrl ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
                {copiedUrl ? t('remote.copied') : t('remote.copy')}
              </button>
            </div>
          )}

          {isRunning && !accessUrl && (
            <div className="flex flex-col items-center justify-center gap-1.5 py-4 text-center">
              <span
                className={cn(
                  'h-1.5 w-1.5 rounded-full',
                  sawUrl ? 'bg-warning' : 'animate-pulse bg-connection'
                )}
              />
              <p className={cn('text-2xs', sawUrl ? 'text-warning' : 'text-muted-foreground')}>
                {sawUrl ? t('remote.disconnected') : t('remote.starting')}
              </p>
            </div>
          )}
        </div>
      </PopoverContent>
    </Popover>
  )
}
