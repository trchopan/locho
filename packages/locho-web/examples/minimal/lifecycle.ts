// Dedicated integration entry point, separate from the interactive example.
import { LochoClient, LochoError } from '../../src/index';

Object.assign(window, { lochoProof: { LochoClient, LochoError } });
