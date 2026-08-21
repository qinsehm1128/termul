import '@fontsource-variable/geist/index.css'
import '@fontsource-variable/inter/index.css'
import '@fontsource-variable/jetbrains-mono/index.css'
import { createRoot } from 'react-dom/client'
import { bootstrapI18n } from '@/i18n/bootstrap'
import TauriApp from './TauriApp'
import './index.css'

async function bootstrap(): Promise<void> {
  await bootstrapI18n()
  createRoot(document.getElementById('root')!).render(<TauriApp />)
}

void bootstrap()
