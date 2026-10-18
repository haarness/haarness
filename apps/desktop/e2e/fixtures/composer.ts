import type { Page } from "@playwright/test";

/** The composer's + menu ("Attach"): "Attach files" opens the system's file picker. */
export async function attachFiles(page: Page) {
  await page.getByRole("button", { name: "Attach", exact: true }).click();
  await page.getByRole("menuitem", { name: "Attach files", exact: true }).click();
}
