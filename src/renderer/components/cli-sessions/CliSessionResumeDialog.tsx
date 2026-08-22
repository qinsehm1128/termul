import type { DiscoveredCliSession } from '@shared/types/cli-session.types'
import { CLI_SESSION_AGENT_LABELS } from '@shared/types/cli-session.types'
import { useState } from 'react'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'

interface CliSessionResumeDialogProps {
  session: DiscoveredCliSession | null
  defaultExtraArgs: string
  busy?: boolean
  onOpenChange: (open: boolean) => void
  onConfirm: (onceExtraArgs: string) => void
}

export function CliSessionResumeDialog({
  session,
  defaultExtraArgs,
  busy = false,
  onOpenChange,
  onConfirm
}: CliSessionResumeDialogProps): React.JSX.Element {
  const [onceExtraArgs, setOnceExtraArgs] = useState('')

  return (
    <Dialog
      open={session !== null}
      onOpenChange={(open) => {
        if (!open) setOnceExtraArgs('')
        onOpenChange(open)
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Resume CLI session</DialogTitle>
          <DialogDescription>
            {session
              ? `${CLI_SESSION_AGENT_LABELS[session.agentId]} · ${session.title}`
              : 'Resume a scanned CLI agent session in a new terminal.'}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          <p className="text-xs text-muted-foreground">
            Default extra args are inserted before the resume flag. One-time args apply only to this
            launch.
          </p>
          <div className="space-y-1.5">
            <Label htmlFor="cli-resume-default-args">Default extra args</Label>
            <Input id="cli-resume-default-args" value={defaultExtraArgs} readOnly />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="cli-resume-once-args">One-time extra args</Label>
            <Input
              id="cli-resume-once-args"
              value={onceExtraArgs}
              onChange={(event) => setOnceExtraArgs(event.target.value)}
              placeholder="--yolo"
            />
          </div>
        </div>
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
            Cancel
          </Button>
          <Button
            type="button"
            disabled={!session || busy}
            onClick={() => onConfirm(onceExtraArgs)}
          >
            Resume
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
