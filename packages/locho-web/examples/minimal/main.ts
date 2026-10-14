import { LochoClient } from '../../src/index';

const output = document.querySelector<HTMLPreElement>('#output')!;
const host = document.querySelector<HTMLInputElement>('#host')!;
const capability = document.querySelector<HTMLInputElement>('#capability')!;
document.querySelector<HTMLButtonElement>('#connect')!.addEventListener('click', async () => {
  output.textContent = 'connecting';
  try {
    const client = await LochoClient.connect({ hostId: host.value.trim() });
    const response = await client.health(capability.value, 'sample-api');
    output.textContent = `peer=${client.status.state}/${client.status.path}\napi=${response.status}\n${await response.text()}`;
    await client.close();
  } catch (error) {
    output.textContent = error instanceof Error ? error.toString() : 'connection failed';
  }
});
