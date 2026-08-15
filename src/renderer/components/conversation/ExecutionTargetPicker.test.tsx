import type { ExecutionTarget, ProjectAttachment } from '@shared/types/conversation.types'
import { fireEvent, render, screen } from '@testing-library/react'
import { useState } from 'react'
import { describe, expect, it } from 'vitest'
import type { Project } from '@/types/project'
import { ExecutionTargetPicker, validateExecutionTarget } from './ExecutionTargetPicker'

if (!HTMLElement.prototype.hasPointerCapture) {
  HTMLElement.prototype.hasPointerCapture = () => false
  HTMLElement.prototype.setPointerCapture = () => undefined
  HTMLElement.prototype.releasePointerCapture = () => undefined
}

if (!HTMLElement.prototype.scrollIntoView) {
  HTMLElement.prototype.scrollIntoView = () => undefined
}

const identity = {
  conversationId: '018f7a1c-1b4d-7c8a-9f01-0123456789ab',
  createdAtUtc: '2026-08-15T09:45:15.123Z',
  creationPartition: { year: 2026, month: 8, day: 15, path: '2026/08/15' },
  workspaceCwd: '/visible/sessions/2026/08/15/conversation'
}

const projects: Project[] = [
  {
    id: 'p1',
    name: 'Termul',
    color: 'blue',
    path: '/projects/termul',
    isGitRepo: true,
    gitBranch: 'main',
    activeWorktreeId: 'w1',
    worktrees: [
      {
        id: 'w1',
        name: 'feature',
        path: '/projects/termul-worktree',
        branch: 'feature/conversations',
        createdAt: '2026-08-15T09:00:00.000Z'
      }
    ]
  }
]

function Harness(): React.JSX.Element {
  const [target, setTarget] = useState<ExecutionTarget>({ kind: 'workspace' })
  const [attachment, setAttachment] = useState<ProjectAttachment | null>(null)
  return (
    <ExecutionTargetPicker
      projects={projects}
      value={target}
      attachment={attachment}
      conversation={identity}
      workspaceCwd={identity.workspaceCwd}
      onChange={setTarget}
      onAttachmentChange={setAttachment}
      nowUtc={() => '2026-08-15T10:00:00.000Z'}
    />
  )
}

async function choose(label: string, option: string): Promise<void> {
  const trigger = screen.getByRole('combobox', { name: label })
  trigger.focus()
  fireEvent.keyDown(trigger, { key: 'ArrowDown' })
  fireEvent.click(await screen.findByRole('option', { name: option }))
}

describe('ExecutionTargetPicker', () => {
  it('defaults to the independent workspace and marks identity as unchanged', () => {
    render(<Harness />)

    expect(screen.getByRole('combobox', { name: 'Execution target' })).toHaveTextContent(
      'Conversation workspace'
    )
    expect(screen.getByTestId('workspace-identity-unchanged')).toHaveAttribute(
      'data-unchanged',
      'true'
    )
    expect(screen.getAllByText(identity.workspaceCwd).length).toBeGreaterThan(0)
    expect(screen.getByText(identity.conversationId)).toBeInTheDocument()
    expect(screen.getByText(identity.createdAtUtc)).toBeInTheDocument()
    expect(screen.getByText(identity.creationPartition.path)).toBeInTheDocument()
    expect(screen.getByText('No project attachment')).toBeInTheDocument()
  })

  it('selects an explicit project root without changing the visible workspace cwd', async () => {
    render(<Harness />)

    await choose('Execution target', 'Project root')

    expect(screen.getByText('/projects/termul')).toBeInTheDocument()
    expect(screen.getAllByText(identity.workspaceCwd).length).toBeGreaterThan(0)
    expect(screen.getByText(identity.conversationId)).toBeInTheDocument()
    expect(screen.getByText(identity.createdAtUtc)).toBeInTheDocument()
    expect(screen.getByText(identity.creationPartition.path)).toBeInTheDocument()
    expect(screen.getByTestId('workspace-identity-unchanged')).toHaveAttribute(
      'data-unchanged',
      'true'
    )
  })

  it('selects an explicit existing worktree and keeps attachment separate', async () => {
    render(<Harness />)

    await choose('Execution target', 'Worktree')
    expect(
      screen.getByText(/\/projects\/termul-worktree · feature\/conversations/)
    ).toBeInTheDocument()
    expect(screen.getByText('No project attachment')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Attach project context' }))
    expect(screen.getByText('Attached project: p1')).toBeInTheDocument()
    expect(screen.getByText(identity.conversationId)).toBeInTheDocument()
    expect(screen.getByText(identity.createdAtUtc)).toBeInTheDocument()
    expect(screen.getByText(identity.creationPartition.path)).toBeInTheDocument()
    expect(screen.getAllByText(identity.workspaceCwd).length).toBeGreaterThan(0)
    fireEvent.click(screen.getByRole('button', { name: 'Detach project context' }))
    expect(screen.getByText('No project attachment')).toBeInTheDocument()
    expect(screen.getByText(identity.conversationId)).toBeInTheDocument()
  })

  it('validates project and worktree selections explicitly', () => {
    expect(validateExecutionTarget({ kind: 'workspace' })).toBeNull()
    expect(
      validateExecutionTarget({ kind: 'project_root', projectId: 'p1', projectRoot: '' })
    ).toBe('projectRootRequired')
    expect(
      validateExecutionTarget({
        kind: 'worktree',
        projectId: 'p1',
        worktreePath: '',
        worktreeBranch: ''
      })
    ).toBe('worktreeBranchRequired')
  })
})
