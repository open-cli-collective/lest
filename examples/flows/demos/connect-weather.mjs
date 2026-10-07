// Connects the sample app to the (fake) SkyCast weather service. The
// consent screen opens in a popup, which is recorded too.
export default async function ({ ui, beat, phase, popup, screenshot }) {
  await phase("dashboard", async () => {
    await ui.waitFor("[data-testid=plant]");
    beat("plants");
    await ui.dwell(1200);
  });
  await phase("consent", async () => {
    const consent = await popup(() => ui.click("#connect"));
    beat("consent");
    await ui.dwell(900);
    await ui.click("#allow");
    await consent.waitForEvent("close");
  });
  await phase("connected", async () => {
    await ui.expectText("#weather-status", "Connected");
    beat("connected");
    await ui.dwell(1500);
    await screenshot("connected");
  });
}
