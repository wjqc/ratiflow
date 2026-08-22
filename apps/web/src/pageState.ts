export type Page = 'entry' | 'workbench' | 'approvals' | 'diagnostics';

import type { Dispatch, SetStateAction } from 'react';

export function navigate(
  setPage: Dispatch<SetStateAction<Page>>,
  page: Page,
  setWorkbenchId: Dispatch<SetStateAction<string>>,
): void {
  if (page === 'entry') {
    setWorkbenchId('');
  }
  setPage(page);
}
