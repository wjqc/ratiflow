import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import AppShell from './app/AppShell';
import '../styles/tokens.css';
import '../styles/workbench.css';
import '../styles/newtask.css';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AppShell />
  </StrictMode>,
);
