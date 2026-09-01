# Plan de consolidation du dispatch IPC et du rescheduling HLE

## Objectif

Corriger le changement de fibre pendant une transaction IPC en deux étapes indépendantes et
testables. La première étape doit supprimer les chemins de dispatch concurrents. La seconde doit
séparer les mises à jour immédiates du scheduler invité de la bascule différée de la fibre hôte.

Chaque étape doit partir de `main`, vivre sur sa propre branche et être validée séparément. La
seconde étape ne doit pas commencer avant les tests manuels et l'intégration de la première dans
`main`.

## État préalable

La branche expérimentale `fix/hle-ipc-safe-reschedule` contient actuellement des modifications non
commitées dans `k_scheduler.rs` et `k_thread.rs`. Elles ne doivent pas être reprises telles quelles
dans la première étape : elles mélangent déjà le rescheduling avec la consolidation du dispatch.

Avant de commencer :

1. Conserver tous les changements utilisateur sans rapport avec ce travail.
2. Retirer uniquement les modifications expérimentales non commitées de `k_scheduler.rs` et
   `k_thread.rs` introduites pour cette investigation.
3. Revenir sur `main`, le mettre au niveau de `origin/main`, puis créer la branche de la première
   étape.

## Étape 1 — Ne garder qu'une implémentation du dispatch IPC

### Branche

Créer `fix/hle-ipc-single-dispatch` directement depuis `main`.

### Problème à corriger

Eden possède plusieurs sources d'événements, mais une seule implémentation de la transaction IPC :
`ServerManager::CompleteSyncRequest`. Les nouvelles requêtes et les reprises de requêtes différées
passent toutes les deux par cette méthode.

Ruzu possède actuellement plusieurs implémentations du traitement :

- le chemin partagé utilisé par les nouvelles requêtes du `ServerManager` ;
- l'ancien chemin utilisé notamment lors de la reprise des requêtes différées ;
- un fallback inline dans le SVC qui appelle directement le handler pour certaines sessions sans
  `ServerManager`.

Ces chemins n'ont pas les mêmes règles de verrouillage ni de scheduling. Une requête différée peut
notamment appeler le handler alors que le mutex global du `ServerManager` est encore détenu.

### Architecture cible

1. Conserver une seule transaction équivalente à
   `Eden::Service::ServerManager::CompleteSyncRequest`, propriétaire de :
   - la remise à zéro de l'état `deferred` ;
   - l'appel unique à `SessionRequestManager::complete_sync_request` ;
   - l'enregistrement d'une nouvelle déférence ;
   - l'écriture et l'envoi de la réponse ;
   - la destruction d'une session fermée ;
   - la remise en attente de la session après une réponse réussie.
2. Faire passer par cette transaction aussi bien :
   - une nouvelle requête reçue par `OnSessionEvent` ;
   - une requête reprise par `OnDeferralEvent`.
3. Adapter la transaction aux contraintes Rust en trois phases sans dupliquer sa logique :
   - sous le mutex du `ServerManager`, détacher ou capturer l'état nécessaire de la session ;
   - relâcher ce mutex avant tout appel au handler IPC ;
   - reprendre le mutex uniquement pour enregistrer la déférence, détruire ou relier la session.
4. Ne jamais conserver le mutex global du `ServerManager` pendant :
   - le callback du service ;
   - l'écriture de la réponse ;
   - `SendReplyHLE` ;
   - une opération susceptible de rescheduler ou de changer de fibre.
5. Ramener le SVC à son rôle upstream : résoudre la session, envoyer/enfiler la requête, attendre la
   réponse et retourner son résultat. Il ne doit pas exécuter directement le handler dans le chemin
   normal.
6. Supprimer le fallback inline en tant que seconde implémentation du dispatch. Les tests qui en
   dépendent doivent être branchés sur un vrai `ServerManager`. Si un adaptateur de test reste
   indispensable, il doit seulement préparer/transporter une requête vers l'unique transaction et
   ne contenir aucune copie de la logique callback/déférence/réponse.
7. Garder cette logique dans `service/server_manager.rs`, conformément à l'ownership de
   `server_manager.cpp`. `svc_ipc.rs` ne doit pas devenir propriétaire du dispatch HLE.

### Vérifications obligatoires

1. Relire `server_manager.h/.cpp`, `svc_ipc.cpp`, `k_server_session.h/.cpp` et leurs contreparties
   Rust après l'implémentation.
2. Vérifier qu'il ne reste qu'un seul propriétaire de la transaction complète. Les appels bas
   niveau au handler peuvent subsister dans `hle_ipc`, mais aucun second enchaînement complet ne
   doit rester dans le SVC ou dans un ancien chemin du `ServerManager`.
3. Ajouter des tests ciblés pour :
   - une requête initiale ;
   - une requête différée puis reprise ;
   - une fermeture de session par le noyau ou par le service ;
   - l'ordre callback, écriture de réponse, `SendReplyHLE`, remise en attente ;
   - l'absence du verrou global du `ServerManager` pendant le callback ;
   - plusieurs requêtes FIFO sur la même session.
4. Adapter les tests SVC qui utilisaient le fallback inline afin qu'ils utilisent le véritable
   routage par `ServerManager`.
5. Lancer les tests ciblés, puis `cargo test -p core` et le build de `ruzu`/`ruzu-cmd`.
6. Comparer une dernière fois ligne par ligne avec Eden et mettre à jour `DIFF.md` avec uniquement
   les adaptations Rust réellement nécessaires.

### Point d'arrêt obligatoire

Une fois cette première branche relue, testée, commitée et poussée :

1. Ne pas commencer l'étape 3.
2. S'arrêter pour permettre les tests manuels de Ruzu.
3. Attendre la confirmation que `fix/hle-ipc-single-dispatch` peut être mergée dans `main`.
4. Laisser le merge dans `main` être effectué ou explicitement autorisé par l'utilisateur.

## Étape 2 — Tests et intégration dans `main`

Valider notamment :

- démarrage et arrêt de plusieurs jeux/homebrews ;
- arrêt de l'émulation sans fermeture de l'interface ;
- fermeture complète de Ruzu ;
- applets et dialogues qui effectuent des IPC imbriquées ;
- requêtes différées ;
- absence de deadlock, de session sans réponse et de régression de performances IPC.

En cas d'échec, corriger uniquement `fix/hle-ipc-single-dispatch`, recommencer les tests et ne pas
ouvrir la branche suivante. Après validation, merger cette branche dans `main` et pousser `main`.

## Étape 3 — Scheduler invité immédiat, bascule de fibre différée

### Branche

Après le merge et le push de l'étape 1, repartir du nouveau `main` et créer
`fix/hle-ipc-deferred-fiber-switch`.

### Principe

Une demande de rescheduling produite pendant un handler IPC ne doit pas être ignorée ni retarder la
mise à jour du scheduler invité. Seule l'opération dangereuse pour la pile hôte — blocage ou bascule
de la fibre courante — doit attendre la fin complète de la transaction IPC.

### Séquence cible

1. Continuer immédiatement les mises à jour du scheduler invité :
   - états des threads ;
   - files de threads exécutables ;
   - priorités et héritages de priorité ;
   - wakeups et attentes ;
   - sélection du prochain thread invité ;
   - demandes de rescheduling des autres cœurs.
2. Lorsqu'une mise à jour implique de bloquer ou de remplacer la fibre hôte courante pendant un
   dispatch IPC, enregistrer une demande de bascule en attente au lieu d'effectuer immédiatement
   `yield`, `jump`, `reschedule_current_hle_thread` ou l'équivalent.
3. Coalescer les demandes imbriquées sans en perdre : un masque de cœurs et/ou un état pending doit
   conserver toute demande faite pendant la transaction.
4. Terminer intégralement la transaction IPC :
   - callback du service terminé ;
   - résultat final déterminé ;
   - réponse écrite dans le buffer invité ;
   - `SendReplyHLE` terminé lorsqu'une réponse doit être envoyée ;
   - état deferred/closed/relinked finalisé ;
   - gardes Rust, emprunts et mutex temporaires détruits.
5. Revenir dans une couche extérieure à la transaction, où aucun verrou IPC temporaire n'est détenu,
   puis consommer la demande en attente et effectuer la bascule ou le blocage de la fibre hôte.

Une tentative de handler qui se termine à nouveau en état différé n'envoie naturellement pas de
réponse. Dans ce cas, la bascule peut être consommée seulement après que l'enregistrement de la
déférence et la destruction de tous les gardes temporaires sont terminés.

### Contraintes d'implémentation

1. Ne pas retarder les changements d'état du noyau invité : cela modifierait la sémantique visible
   par les autres cœurs et pourrait introduire de la latence.
2. Ne pas exécuter la bascule depuis le `Drop` d'une garde susceptible d'être détruite alors qu'un
   autre mutex est encore vivant. Structurer les scopes pour que la transaction retourne d'abord,
   puis effectuer explicitement la bascule depuis son appelant extérieur.
3. Gérer l'imbrication avec une profondeur de transaction ou un mécanisme équivalent : seul le
   niveau extérieur peut consommer le rescheduling hôte en attente.
4. Préserver les demandes cross-core immédiatement ; la déférence concerne uniquement la fibre hôte
   courante qui exécute le callback IPC.
5. Couvrir tous les résultats de transaction : succès, nouvelle déférence, fermeture de session,
   erreur et arrêt de l'émulateur.
6. Conserver `fcontext` comme moteur de fibres. Cette étape modifie le moment de la bascule, pas son
   implémentation bas niveau.
7. Comparer le comportement du scheduler avec Eden et avec les invariants du noyau Switch. Toute
   adaptation imposée par les fibres et mutex Rust doit rester locale, explicite et documentée.

### Tests ciblés

Ajouter des tests qui prouvent séparément que :

1. Un handler peut demander un rescheduling et les états invités sont mis à jour avant son retour.
2. Aucune bascule de fibre hôte ne survient avant la fin du callback.
3. La réponse est écrite et `SendReplyHLE` est terminé avant la bascule.
4. Tous les mutex et gardes temporaires sont libérés au moment de la bascule.
5. Plusieurs demandes imbriquées sont coalescées puis consommées une seule fois au niveau extérieur.
6. Les demandes destinées aux autres cœurs ne sont pas retardées.
7. Les chemins deferred, session closed, erreur et shutdown ne perdent pas la demande pending.
8. Le client reprend correctement après la réponse et aucun thread ne reste bloqué à la fermeture.

Terminer par les tests ciblés, `cargo test -p core`, le build de `ruzu` et `ruzu-cmd`, une comparaison
ligne par ligne avec Eden et la mise à jour de `DIFF.md`.

## Critère de succès global

- Une seule implémentation de la transaction de dispatch IPC existe.
- Le callback IPC n'est jamais exécuté sous le mutex global du `ServerManager`.
- Les mises à jour du scheduler invité restent immédiates.
- Le blocage ou changement de fibre hôte n'arrive qu'après la finalisation et le déverrouillage
  complets de la transaction IPC.
- Les deux étapes restent indépendantes dans l'historique Git et peuvent être testées ou annulées
  séparément.
