// Dedicated integration entry point, separate from the interactive example.
import { LochoClient, LochoError, LochoTicket } from '../../src/index';

Object.assign(window, { lochoProof: { LochoClient, LochoError, LochoTicket } });
