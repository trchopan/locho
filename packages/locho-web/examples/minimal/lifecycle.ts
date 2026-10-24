// Dedicated integration entry point, separate from the interactive example.
/// <reference types="vite/client" />
import { LochoClient, LochoError, LochoTicket } from '../../src/index';
import metadataWorkerUrl from './metadata-worker?worker&url';
import metadataSoakWorkerUrl from './metadata-soak-worker?worker&url';
import { MetadataSoak } from './metadata-soak';

Object.assign(window, { lochoProof: { LochoClient, LochoError, LochoTicket, metadataWorkerUrl, MetadataSoak, metadataSoakWorkerUrl } });
