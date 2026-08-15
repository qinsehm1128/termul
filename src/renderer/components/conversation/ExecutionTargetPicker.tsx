import type {
  ConversationRecordV2,
  ExecutionTarget,
  ProjectAttachment
} from '@shared/types/conversation.types'
import { Folder, FolderGit2, Link2, PanelsTopLeft, Unlink } from 'lucide-react'
import { useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue
} from '@/components/ui/select'
import type { Project } from '@/types/project'

export interface ExecutionTargetPickerProps {
  projects: readonly Project[]
  value: ExecutionTarget
  attachment: ProjectAttachment | null
  conversation?: Pick<
    ConversationRecordV2,
    'conversationId' | 'createdAtUtc' | 'creationPartition' | 'workspaceCwd'
  > | null
  workspaceCwd?: string | null
  onChange: (target: ExecutionTarget) => void
  onAttachmentChange: (attachment: ProjectAttachment | null) => void
  nowUtc?: () => string
}

function projectRoot(project: Project | undefined): string {
  return project?.path?.trim() ?? ''
}

function activeWorktree(project: Project | undefined) {
  if (!project?.activeWorktreeId) return undefined
  return project.worktrees?.find((worktree) => worktree.id === project.activeWorktreeId)
}

export type ExecutionTargetValidationError =
  | 'projectRequired'
  | 'projectRootRequired'
  | 'worktreeBranchRequired'

export function validateExecutionTarget(
  target: ExecutionTarget
): ExecutionTargetValidationError | null {
  if (target.kind === 'workspace') return null
  if (!target.projectId.trim()) return 'projectRequired'
  if (target.kind === 'project_root') {
    return target.projectRoot.trim() ? null : 'projectRootRequired'
  }
  if (!target.worktreeBranch.trim()) return 'worktreeBranchRequired'
  return null
}

export function ExecutionTargetPicker({
  projects,
  value,
  attachment,
  conversation,
  workspaceCwd,
  onChange,
  onAttachmentChange,
  nowUtc = () => new Date().toISOString()
}: ExecutionTargetPickerProps): React.JSX.Element {
  const { t } = useTranslation('conversation')
  const selectedProjectId = value.kind === 'workspace' ? (projects[0]?.id ?? '') : value.projectId
  const selectedProject = projects.find((project) => project.id === selectedProjectId)
  const validationError = validateExecutionTarget(value)
  const attachableProjects = useMemo(
    () => projects.filter((project) => Boolean(project.path?.trim())),
    [projects]
  )

  const selectKind = (kind: ExecutionTarget['kind']): void => {
    if (kind === 'workspace') {
      onChange({ kind: 'workspace' })
      return
    }
    const project = selectedProject ?? attachableProjects[0]
    if (kind === 'project_root') {
      onChange({
        kind,
        projectId: project?.id ?? '',
        projectRoot: projectRoot(project)
      })
      return
    }
    const selectedWorktree = activeWorktree(project)
    onChange({
      kind,
      projectId: project?.id ?? '',
      worktreePath: selectedWorktree?.path ?? '',
      worktreeBranch: selectedWorktree?.branch ?? project?.gitBranch ?? ''
    })
  }

  const selectProject = (projectId: string): void => {
    const project = projects.find((candidate) => candidate.id === projectId)
    if (!project || value.kind === 'workspace') return
    if (value.kind === 'project_root') {
      onChange({ kind: value.kind, projectId, projectRoot: projectRoot(project) })
      return
    }
    const selectedWorktree = activeWorktree(project)
    onChange({
      kind: value.kind,
      projectId,
      worktreePath: selectedWorktree?.path ?? '',
      worktreeBranch: selectedWorktree?.branch ?? project.gitBranch ?? ''
    })
  }

  const toggleAttachment = (): void => {
    if (attachment) {
      onAttachmentChange(null)
      return
    }
    const project = selectedProject ?? attachableProjects[0]
    const root = projectRoot(project)
    if (!project || !root) return
    const selectedWorktree = value.kind === 'worktree' ? activeWorktree(project) : undefined
    onAttachmentChange({
      schemaVersion: 1,
      projectId: project.id,
      attachedAtUtc: nowUtc(),
      projectPathSnapshot: root,
      worktreePath: selectedWorktree?.path ?? null,
      worktreeBranch: selectedWorktree?.branch ?? null
    })
  }

  return (
    <section
      className="rounded-xl border border-border/60 bg-card/60 p-3"
      aria-label={t('target.title')}
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-1.5">
          <Label htmlFor="execution-target-kind">{t('target.label')}</Label>
          <Select
            value={value.kind}
            onValueChange={(kind) => selectKind(kind as ExecutionTarget['kind'])}
          >
            <SelectTrigger id="execution-target-kind" aria-label={t('target.label')}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="workspace">
                <span className="flex items-center gap-2">
                  <PanelsTopLeft className="size-4" aria-hidden="true" />
                  {t('target.workspace')}
                </span>
              </SelectItem>
              <SelectItem value="project_root" disabled={attachableProjects.length === 0}>
                <span className="flex items-center gap-2">
                  <Folder className="size-4" aria-hidden="true" />
                  {t('target.projectRoot')}
                </span>
              </SelectItem>
              <SelectItem value="worktree" disabled={attachableProjects.length === 0}>
                <span className="flex items-center gap-2">
                  <FolderGit2 className="size-4" aria-hidden="true" />
                  {t('target.worktree')}
                </span>
              </SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div className="space-y-1.5">
          <Label htmlFor="execution-target-project">{t('target.project')}</Label>
          <Select
            value={selectedProjectId}
            onValueChange={selectProject}
            disabled={value.kind === 'workspace' || attachableProjects.length === 0}
          >
            <SelectTrigger id="execution-target-project" aria-label={t('target.project')}>
              <SelectValue placeholder={t('target.noProject')} />
            </SelectTrigger>
            <SelectContent>
              {attachableProjects.map((project) => (
                <SelectItem key={project.id} value={project.id}>
                  {project.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>

      <div className="mt-3 flex flex-col gap-2 text-xs text-muted-foreground sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <div data-testid="workspace-identity-unchanged" data-unchanged="true">
            {t('target.workspaceUnchanged')}
          </div>
          <code className="block truncate font-mono" title={workspaceCwd ?? undefined}>
            {workspaceCwd || t('target.newWorkspace')}
          </code>
          {conversation ? (
            <dl
              className="mt-2 grid gap-x-3 gap-y-1 sm:grid-cols-[auto_minmax(0,1fr)]"
              data-testid="conversation-identity-details"
            >
              <dt>{t('identity.conversationId')}</dt>
              <dd className="truncate font-mono" title={conversation.conversationId}>
                {conversation.conversationId}
              </dd>
              <dt>{t('identity.createdAtUtc')}</dt>
              <dd className="truncate font-mono" title={conversation.createdAtUtc}>
                {conversation.createdAtUtc}
              </dd>
              <dt>{t('identity.creationPartition')}</dt>
              <dd className="truncate font-mono" title={conversation.creationPartition.path}>
                {conversation.creationPartition.path}
              </dd>
              <dt>{t('identity.workspaceCwd')}</dt>
              <dd className="truncate font-mono" title={conversation.workspaceCwd}>
                {conversation.workspaceCwd}
              </dd>
            </dl>
          ) : null}
          {value.kind === 'project_root' ? (
            <code className="block truncate font-mono">
              {value.projectRoot || t('target.invalid')}
            </code>
          ) : null}
          {value.kind === 'worktree' ? (
            <code className="block truncate font-mono">
              {value.worktreePath || t('target.newWorktree')} ·{' '}
              {value.worktreeBranch || t('target.invalid')}
            </code>
          ) : null}
          {validationError ? (
            <p role="alert" className="mt-1 text-destructive">
              {t(`target.errors.${validationError}` as const)}
            </p>
          ) : null}
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="min-h-10 shrink-0 gap-2"
          disabled={!attachment && attachableProjects.length === 0}
          onClick={toggleAttachment}
        >
          {attachment ? <Unlink className="size-4" /> : <Link2 className="size-4" />}
          {attachment ? t('attachment.detach') : t('attachment.attach')}
        </Button>
      </div>

      <output className="mt-2 block text-xs text-muted-foreground" aria-live="polite">
        {attachment
          ? t('attachment.attached', { projectId: attachment.projectId })
          : t('attachment.none')}
      </output>
    </section>
  )
}
